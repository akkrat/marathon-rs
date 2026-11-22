use strict;
use warnings;
use Time::HiRes qw(clock_gettime CLOCK_MONOTONIC setitimer ITIMER_REAL);
use Fcntl qw(:DEFAULT :flock);
use Errno qw(ENOENT ESRCH EAGAIN EWOULDBLOCK EINTR);

# Only test hooks move this, as if the owner had been descheduled.
my $skew = 0;
sub now_ms { clock_gettime(CLOCK_MONOTONIC) * 1000 + $skew }
sub clock_ms { int(now_ms()) }

# Test hooks. The shipped script never sets one, so every branch that reads
# them is dead; the tests run a copy in which this line fills the table. The
# table is read only through `hook`, which returns a copy.
my %hook;
sub hook { return $hook{$_[0]} }

sub readable {
    my ($file, $delay) = @_;
    my $bits = '';
    vec($bits, fileno($file), 1) = 1;
    return select($bits, undef, undef, $delay) > 0;
}

sub read_state {
    my ($path) = @_;
    my $file;
    if (!open $file, '<', $path) {
        return () if $! == ENOENT;
        die "read state: $!";
    }
    my $state = <$file>;
    defined($state) && $state =~ /\A([0-3]) (\d+) ([0-3]) ([-a-z0-9:]+)\n\z/
        or die "invalid state";
    return ($1, $2, $3, $4);
}

sub locked {
    my ($token, $op, $notify) = @_;
    my @receipt = read_state("$token.receipt");
    if (@receipt) {
        return ($op->(@receipt))[1];
    }
    sysopen my $lock, "$token.lock", O_RDWR|O_CREAT|O_NOFOLLOW, 0600 or die "open lock: $!";
    if (!flock($lock, LOCK_EX | LOCK_NB)) {
        return undef if $! == EAGAIN || $! == EWOULDBLOCK;
        die "lock token: $!";
    }
    # A late caller can open a fresh lock inode after a terminal writer unlinks
    # its lock. Check the immutable receipt again before reading mutable state.
    @receipt = read_state("$token.receipt");
    if (@receipt) {
        unlink "$token.lock";
        return ($op->(@receipt))[1];
    }
    my @state = read_state("$token.state");
    my ($new, $result) = $op->(@state ? @state : (0, 0, 0, '-'));
    if (defined $new) {
        open my $file, '>', "$token.pending" or die "write state: $!";
        print {$file} join(' ', @$new), "\n" or die "write state: $!";
        close $file or die "close state: $!";
        my $done = $new->[3] ne '-';
        # Before the commit and under the lock: see wake.
        $notify->($new) if $notify;
        my $suffix = $done ? 'receipt' : 'state';
        rename "$token.pending", "$token.$suffix" or die "commit state: $!";
        # Test hook: a control request killed between its commit and its exit.
        kill 'KILL', $$ if $notify && hook('kill_after_commit');
        unlink "$token.state", "$token.lock" if $done;
    }
    close $lock or die "close lock: $!";
    return $result;
}

sub control {
    my ($dir, $op, $lease) = @_;
    my $recorder;
    my $reply = locked($dir, sub {
        my ($started, $expiry, $signal, $receipt) = @_;
        # 2 is a recorder, 3 a recorder that has printed its start marker.
        $recorder = $started >= 2;
        my $now = clock_ms();
        my $running = ($started == 3 ? 'recording' : 'running') . ":$expiry";
        return (undef, $receipt) if $receipt ne '-';
        if ($op =~ /\A[1-3]\z/) {
            return ([0, 0, $op, 'not-started'], 'not-started') if !$started;
            $signal = $op if $op > $signal;
        } elsif ($op eq 'status') {
            return (undef, $started ? $running : 'unknown');
        } elsif ($op =~ /\Arenew-([0-9]+)\z/ && $started && $expiry == $1 && $expiry > $now && $signal < 2) {
            # Retries must carry the same stamp. At most one queued renewal can
            # extend an expiry the controller actually observed.
            my $proposed = $now + $lease;
            $expiry = $proposed if $proposed > $expiry;
            $running = ($started == 3 ? 'recording' : 'running') . ":$expiry";
        }
        return ([$started, $expiry, $signal, '-'], $started ? $running : 'unknown');
    }, sub {
        # Every change to a running token wakes its owner; status requests
        # change nothing and terminal states have no owner left.
        my ($new) = @_;
        wake($dir) if $new->[0] && $new->[3] eq '-';
    }) // 'busy';
    if ($reply =~ /\A(?:done:[1-9][0-9]*|unconfirmed)\z/ && open my $file, '<', "$dir.stderr") {
        my $count = read($file, my $diagnostic, 8192);
        if ($count) {
            require MIME::Base64;
            $reply .= ' ' . MIME::Base64::encode_base64($diagnostic, '');
        }
    }
    return ($reply, $recorder);
}

# The owner of a running token reads this FIFO. A control request writes its
# byte while it holds the token lock, before it commits the state. Any state
# read that succeeds after the byte was written therefore follows the commit
# (or the writer's death without one): a woken owner sees the change or finds
# the lock busy and retries (1 ms, then every 10 ms). A byte the owner has not
# drained keeps its next select from sleeping.
sub wake {
    my ($dir) = @_;
    my $fifo;
    until (sysopen $fifo, "$dir.wake", O_WRONLY|O_NONBLOCK|O_NOFOLLOW) {
        next if $! == EINTR;
        # No FIFO (ENOENT) or no reader (ENXIO): the owner's loop has ended, or
        # it opens the FIFO later than this request took the lock and reads the
        # state after that. Anything else leaves the change to the fallback
        # poll, as before the FIFO existed.
        return;
    }
    # The owner may close its end between this open and the write.
    local $SIG{PIPE} = 'IGNORE';
    if (-p $fifo) {
        # One byte is written whole or not at all. EAGAIN means the FIFO is
        # full, so a wake is already pending; EPIPE that the owner is gone.
        until (defined syswrite($fifo, 'W', 1)) {
            last unless $! == EINTR;
        }
    }
    close $fifo;
}

sub open_wake {
    my ($dir) = @_;
    require POSIX;
    POSIX::mkfifo("$dir.wake", 0600) or return;
    # The owner's own writer keeps the reader from seeing end of file.
    my ($wake, $keep);
    if (sysopen($wake, "$dir.wake", O_RDONLY|O_NONBLOCK|O_NOFOLLOW) && -p $wake
        && sysopen($keep, "$dir.wake", O_WRONLY|O_NONBLOCK|O_NOFOLLOW)) {
        return ($wake, $keep);
    }
    unlink "$dir.wake";
    return;
}

# The time in ms until the owner's next pass and what it waits for: the lease
# expiry until the pass has acted on it (force), a recorder's TERM deadline (2 s
# after the first stop request or expiry, until TERM is sent) and KILL deadline
# (1 s after TERM when forced), a busy retry ($retry ms, 0 for none), or the
# fallback poll. A deadline that is due or past is never dropped: it gets the
# floor. Deadlines win ties; never below 10 ms except for a shorter retry.
sub next_wake {
    my ($now, $expiry, $interrupt_at, $term_at, $stopping, $force, $retry, $poll) = @_;
    my @due = ([$poll, 'p']);
    push @due, [$retry, 'b'] if $retry;
    push @due, [$expiry - $now, 'd'] if !$force;
    if (defined $interrupt_at) {
        push @due, [$interrupt_at + 2000 - $now, 'd'] if $stopping && !defined $term_at;
        push @due, [$term_at + 1000 - $now, 'd'] if $force && defined $term_at;
    }
    my ($delay, $cause) = @{shift @due};
    for (@due) {
        ($delay, $cause) = @$_ if $_->[0] <= $delay;
    }
    my $floor = $retry && $retry < 10 ? $retry : 10;
    return ($delay < $floor ? $floor : $delay, $cause);
}

# Set by SIGALRM, which only the timer of `arm` sends.
my $alarmed = 0;

# Arms the kernel's interval timer to end the owner's sleep at $at ms on the
# monotonic clock and returns the ms left until then (0 when due). select's
# own timeout is relative: an owner descheduled between choosing it and
# entering select would sleep all of it after resuming. The timer runs on while
# the owner is descheduled, so an owner resuming past $at finds its SIGALRM
# pending or $alarmed set and does not sleep. The timer is set from a clock
# reading taken just before; a reading just after bounds how late that makes
# it, and it is set again if more than 1 ms passed between them. Once fired it
# fires every 10 ms until `disarm`, because Perl runs a handler only between
# operations: a SIGALRM after the owner checked $alarmed but before it entered
# select delays the wake by at most those 10 ms.
sub arm {
    my ($at) = @_;
    $alarmed = 0;
    my $left;
    for (1 .. 5) {
        my $before = now_ms();
        return 0 if $at - $before < 0.01;
        # In list context setitimer returns nothing on failure; the relative
        # timeout is then all there is, as before the timer.
        my @armed = setitimer(ITIMER_REAL, ($at - $before) / 1000, 0.01);
        return $at - $before unless @armed;
        $left = $at - now_ms();
        last if $at - $before - $left <= 1;
    }
    return $left > 0 ? $left : 0;
}

sub disarm { setitimer(ITIMER_REAL, 0) }

# Test hook: the owner stops itself here, as if descheduled, until the test
# continues it. The test learns its pid and how far its lease expiry is.
sub descheduled {
    my ($expiry) = @_;
    my $path = hook('stop_file');
    open my $file, '>', "$path.new" or die "write stop file: $!";
    print {$file} "$$ ", $expiry - clock_ms(), "\n";
    close $file or die "close stop file: $!";
    rename "$path.new", $path or die "commit stop file: $!";
    kill 'STOP', $$;
}

sub drain_errors {
    my ($pipe, $tail, $line, $confirmed, $closed) = @_;
    for (1..8) {
        last unless readable($pipe, 0);
        my $count = sysread($pipe, my $bytes, 4096);
        if (!$count) {
            $$closed = 1 if defined $count && $closed;
            last;
        }
        my $remaining = 8192 - length($$tail);
        $$tail .= substr($bytes, 0, $remaining) if $remaining > 0;
        next if $$confirmed;
        # simctl prints this line on stderr once the simulator's video service
        # has started. Only a whole line counts, never a substring of an error.
        $$line .= $bytes;
        while ($$line =~ s/\A([^\n]*)\n//) {
            if ($1 =~ /\ARecording started\r?\z/) {
                $$confirmed = 1;
                last;
            }
        }
        $$line = substr($$line, -256) if length($$line) > 256;
    }
}

sub control_stream {
    my ($root, $lease, $lock) = @_;
    my $buffer = '';
    my %waiting;
    local $| = 1;
    # Only requests hold the shared installation lock. An idle channel must
    # not stop a separate session from retiring this worker root.
    flock($lock, LOCK_UN) or die "unlock supervisor: $!";
    while (1) {
        if (readable(\*STDIN, %waiting ? 0.05 : undef)) {
            my $count = sysread(STDIN, my $bytes, 4096);
            return unless $count;
            $buffer .= $bytes;
        }
        flock($lock, LOCK_SH | LOCK_NB) or die "supervisor is retiring: $!";
        -f "$root/supervisor.pl" or die "supervisor retired";
        my @replies;
        while ($buffer =~ s/\A([^\n]*)\n//) {
            my $frame = $1;
            $frame =~ /\A([1-9][0-9]{0,19}) ([a-f0-9]{32}):(renew-[0-9]{1,16}|status|[1-3])\z/
                or die "invalid control frame";
            my ($id, $token, $op) = ($1, $2, $3);
            my ($state, $recorder) = control("$root/$token", $op, $lease);
            if ($recorder && $op eq '1' && $state =~ /\A(?:running|recording):/) {
                # INT for a recorder can return its completed receipt on the
                # same request without stalling other tokens on this stream.
                if (my $previous = $waiting{$token}) {
                    push @replies, "$previous $token $state\n";
                }
                $waiting{$token} = $id;
            } else {
                push @replies, "$id $token $state\n";
            }
        }
        length($buffer) < 256 or die "control frame too large";
        for my $token (keys %waiting) {
            my ($state) = control("$root/$token", 'status', $lease);
            next if $state =~ /\A(?:running:|recording:|busy)/;
            my $id = delete $waiting{$token};
            push @replies, "$id $token $state\n";
        }
        flock($lock, LOCK_UN) or die "unlock supervisor: $!";
        print @replies or die "write control reply: $!" if @replies;
    }
}

sub run_owned {
    my ($dir, $lease, $announce, @argv) = @_;
    my $recorder = defined $announce;
    my $start;
    my $admission_deadline = clock_ms() + $lease;
    while (!defined $start && clock_ms() < $admission_deadline) {
        $start = locked($dir, sub {
            my ($started, $expiry, $signal, $receipt) = @_;
            return (undef, 0) if $started || $signal || $receipt ne '-';
            my $new_expiry = clock_ms() + $lease;
            return ([$recorder ? 2 : 1, $new_expiry, 0, '-'], $new_expiry);
        });
        select undef, undef, undef, 0.01 if !defined $start;
    }
    return 125 unless $start;

    pipe(my $ready, my $ready_w) or die "ready pipe: $!";
    pipe(my $go, my $go_w) or die "start pipe: $!";
    pipe(my $status, my $status_w) or die "status pipe: $!";
    my ($errors, $errors_w);
    pipe($errors, $errors_w) or die "recorder stderr pipe: $!" if $recorder;
    local $SIG{CHLD} = 'DEFAULT';
    local $SIG{PIPE} = 'IGNORE';
    local $SIG{HUP} = 'IGNORE';
    local $SIG{INT} = 'IGNORE';
    local $SIG{TERM} = 'IGNORE';
    my $anchor = fork();
    defined($anchor) or die "fork anchor: $!";
    if ($anchor == 0) {
        close $announce if $announce;
        close $errors if $recorder;
        close $_ for ($ready, $go_w, $status);
        my $group_permit;
        sysread($go, $group_permit, 1) == 1 && $group_permit eq 'P' or exit(125);
        my $payload = fork();
        defined($payload) or exit(125);
        if ($payload == 0) {
            close $status_w;
            if ($recorder) {
                open STDERR, '>&', $errors_w or exit(125);
                close $errors_w;
            }
            $SIG{$_} = 'DEFAULT' for qw(PIPE HUP INT TERM);
            syswrite($ready_w, 'R', 1) == 1 or exit(125);
            close $ready_w;
            my $permit;
            sysread($go, $permit, 1) == 1 && $permit eq 'G' or exit(125);
            close $go;
            exec { $argv[0] } @argv or exit(127);
        }
        close $errors_w if $recorder;
        close $_ for ($ready_w, $go);
        waitpid($payload, 0) == $payload or exit(125);
        syswrite($status_w, pack('N', $?), 4);
        close $status_w;
        # The supervisor sends group KILL before reaping this member.
        sleep 86400 while 1;
    }

    close $_ for ($ready_w, $go, $status_w);
    close $errors_w if $recorder;
    my $group = setpgrp($anchor, $anchor);
    my $permitted = $group && syswrite($go_w, 'P', 1) == 1;
    # Short commands end before the wake FIFO is due and never pay for it
    # (loading POSIX and the FIFO cost more than their few 25 ms polls).
    my ($wake, $wake_w);
    my $wake_due = clock_ms() + 250;
    my $rc = 125;
    my $seen = 0;
    my ($interrupt_at, $term_at, $interrupted, $stopping);
    my ($diagnostic, $line, $confirmed, $published, $errors_closed) = ('', '', 0, 0, 0);
    my ($expiry, $wanted) = ($start, 0);
    my $starting = 1;
    my $woken = 0;
    # Test hook: one letter per pass of the loop, by what woke it.
    my $trace = defined hook('wakes') ? '' : undef;
    # Test hook: a longer fallback poll, so a witness can tell a wake from a poll.
    my $fallback = hook('fallback_ms') // 1000;
    my ($passes_open, $late) = (0, 0);
    # Only the timer of `arm` sends SIGALRM, and only between `arm` and
    # `disarm`, which enclose nothing but the sleep (and test hooks), so no
    # other call of the owner can be interrupted. Set after the fork: neither
    # the anchor nor the payload has this handler or the timer.
    local $SIG{ALRM} = sub { $alarmed = 1 };
    eval {
        while ($permitted) {
            # Opened after the fork, so neither the anchor nor the payload
            # holds it, and before this pass reads the state, so a request
            # committed before it existed is seen now.
            if (defined $wake_due && clock_ms() >= $wake_due) {
                ($wake, $wake_w) = open_wake($dir);
                undef $wake_due;
            }
            # The first pass with the FIFO open; only test hooks ask.
            my $first_open = $wake && !$passes_open++;
            # Without the FIFO the state is polled as often as before it existed.
            my $poll = $wake ? $fallback : 25;
            # An exit the anchor has reported wins over anything this pass
            # would decide: a payload that ended before a lease expiry or a
            # KILL deadline that the owner only notices now (it was
            # descheduled) keeps its own status. This is the only status read,
            # so every pass that finds the payload ended takes its status
            # before it reads the clock.
            if (readable($status, 0)) {
                my $n = sysread($status, my $packet, 4);
                if ($n && $n == 4) {
                    my $wait_status = unpack('N', $packet);
                    $rc = ($wait_status & 127) ? 128 + ($wait_status & 127) : $wait_status >> 8;
                }
                last;
            }
            my $observed = locked($dir, sub { (undef, [$_[1], $_[2]]) });
            ($expiry, $wanted) = @$observed if defined $observed;
            my $now = clock_ms();
            # Test hook: the lease expiry comes this many ms after this pass's
            # clock sample, as if the lease had been that short.
            if ($first_open && defined hook('expiry_in')) {
                $skew += $expiry - hook('expiry_in') - $now;
                $now = $expiry - hook('expiry_in');
            }
            my $force = $now >= $expiry || $wanted == 3;
            if ($force && (!$recorder || $starting)) {
                $rc = 137;
                last;
            }
            if ($starting && readable($ready, 0)) {
                $starting = 0;
                my $n = sysread($ready, my $byte, 1);
                last if !$n || $byte ne 'R';
                if ($wanted) {
                    $rc = $wanted == 1 ? 130 : 143;
                    last;
                }
                syswrite($go_w, 'G', 1) == 1 or last;
                close $go_w;
                if ($announce) {
                    syswrite($announce, 'G', 1);
                    close $announce;
                }
            }
            if (!$starting) {
                if ($recorder) {
                    drain_errors($errors, \$diagnostic, \$line, \$confirmed, \$errors_closed);
                    if ($confirmed && !$published) {
                        $published = locked($dir, sub {
                            my ($started, $expiry, $signal, $receipt) = @_;
                            return (undef, 1) if $receipt ne '-';
                            return ([3, $expiry, $signal, '-'], 1);
                        });
                    }
                    $stopping = $wanted >= 2 || $force;
                    # A disappearing controller can still leave the simulator's
                    # video service busy. Give the recorder the same first
                    # chance to finalize even on a hard stop or lease expiry.
                    # A signal before the start marker leaves that service
                    # recording with no client, so INT waits for the marker.
                    # TERM and KILL keep their times from the first request.
                    if ($wanted || $force) {
                        $interrupt_at //= $now;
                        if (!$interrupted && $confirmed) {
                            kill 'INT', -$anchor;
                            $interrupted = 1;
                        }
                    }
                    if ($stopping && !defined $term_at && $now - $interrupt_at >= 2000) {
                        kill 'TERM', -$anchor;
                        $term_at = $now;
                    } elsif ($force && defined $term_at && $now - $term_at >= 1000) {
                        $rc = 137;
                        last;
                    }
                } elsif ($wanted > $seen) {
                    kill($wanted == 1 ? 'INT' : 'TERM', -$anchor);
                    $seen = $wanted;
                }
            }
            # Sleep until a pipe or the wake FIFO has something, the next
            # deadline, or the fallback poll, whichever comes first.
            # A busy token lock is retried soon, as is a start not yet published.
            # A request that has just woken the owner holds the lock only until
            # its commit, a moment later, so the first retry after a wake comes
            # sooner; a lock held longer is retried every 10 ms.
            my $retry = !defined $observed || ($confirmed && !$published) ? ($woken ? 1 : 10) : 0;
            # Test hook: the owner is descheduled across the lease expiry
            # between this pass's checks and its sleep.
            if ($first_open && hook('pause_across_expiry') && $now < $expiry) {
                my $ahead = $expiry + 5 - clock_ms();
                $skew += $ahead if $ahead > 0;
                $trace .= '!' if defined $trace;
            }
            # The pass's own clock sample, the one its checks used, chooses
            # what to wait for, so a deadline those checks found ahead is
            # never dropped, and the sleep ends at that sample plus the delay.
            my ($delay, $cause) = next_wake($now, $expiry, $interrupt_at, $term_at, $stopping, $force,
                $retry, $poll);
            my $stop_at = $first_open ? hook('stop_at') // '' : '';
            if ($stop_at eq 'arm') {
                descheduled($expiry);
                $trace .= '!' if defined $trace;
            }
            my $left = arm($now + $delay);
            if ($stop_at eq 'select') {
                descheduled($expiry);
                $trace .= '!' if defined $trace;
            }
            my $bits = '';
            vec($bits, fileno($status), 1) = 1;
            vec($bits, fileno($ready), 1) = 1 if $starting;
            vec($bits, fileno($errors), 1) = 1 if $recorder && !$starting && !$errors_closed;
            vec($bits, fileno($wake), 1) = 1 if $wake;
            # Test hook: tells the test this pass has read the state and sleeps next.
            if ($first_open && hook('parked') && open my $file, '>', hook('parked')) {
                close $file;
            }
            # A SIGALRM interrupts the sleep (EINTR); a deadline cause stays.
            my $found = select(my $out = $bits, undef, undef, ($alarmed ? 0 : $left) / 1000);
            disarm();
            $woken = 0;
            if ($found > 0) {
                $cause = 'e';
                if ($wake && vec($out, fileno($wake), 1)) {
                    ($cause, $woken) = ('w', 1);
                    1 while sysread($wake, my $bytes, 64);
                }
                # Test hook: the owner is descheduled for this many ms right
                # after finding that the payload has ended.
                if (hook('late_after_exit') && vec($out, fileno($status), 1) && !$late++) {
                    $skew += hook('late_after_exit');
                    $trace .= '!' if defined $trace;
                }
            }
            $trace .= $cause if defined $trace;
        }
    };
    # Whatever ended the loop, no timer outlives it.
    disarm();

    close $_ for ($ready, $status);
    if ($wake) {
        close $_ for ($wake, $wake_w);
        unlink "$dir.wake";
    }
    # The unreaped anchor pins this PGID even if its own launch failed.
    kill 'KILL', ($group ? -$anchor : $anchor);
    my $reaped = waitpid($anchor, 0) == $anchor;
    my $end = clock_ms() + 5000;
    my $gone = 0;
    while ($group) {
        $gone = $group && !(kill 0, -$anchor) && $! == ESRCH;
        last if $gone || clock_ms() >= $end;
        select undef, undef, undef, 0.025;
    }
    if ($recorder) {
        drain_errors($errors, \$diagnostic, \$line, \$confirmed);
        close $errors;
        if (length($diagnostic) && ($rc || !$reaped || !$gone)) {
            # Diagnostics are published before the terminal receipt.
            if (sysopen my $file, "$dir.stderr", O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW, 0600) {
                print {$file} $diagnostic;
                close $file;
            }
        }
    }
    if (defined $trace && open my $file, '>>', hook('wakes')) {
        print {$file} "$trace\n";
        close $file;
    }
    my $receipt_deadline = clock_ms() + 5000;
    my $recorded;
    while (clock_ms() < $receipt_deadline) {
        $recorded = locked($dir, sub {
            return ([$_[0], $_[1], $_[2], $reaped && $gone ? "done:$rc" : 'unconfirmed'], 1);
        });
        last if $recorded;
        select undef, undef, undef, 0.025;
    }
    die "worker cleanup was not proved" unless $reaped && $gone && $recorded;
    return $rc;
}

sub exit_owned {
    my $status = eval { run_owned(@_) };
    exit $status if defined $status;
    # OpenSSH must report exit-signal, never a successful cleanup status.
    # The controller may still obtain the terminal receipt over control.
    kill 'KILL', $$;
    exit(125);
}

# Loaded with `do` (the timeout tests), only the subroutines are defined.
return 1 if caller;
my $mode = shift @ARGV // die "no mode";
if ($mode eq 'init') {
    require File::Temp;
    print File::Temp::tempdir('marathon-supervisor-XXXXXXXXXX', DIR => '/tmp', CLEANUP => 0), "\n";
    exit 0;
}
my ($root, $lease) = splice @ARGV, 0, 2;
$root =~ m{\A/tmp/marathon-supervisor-[A-Za-z0-9_-]+\z} or die "invalid root";
my @stat = lstat($root);
-d _ && $stat[4] == $< && ($stat[2] & 0777) == 0700 or die "invalid private root";
$lease =~ /\A[1-9][0-9]*\z/ && $lease <= 3600000 or die "invalid lease";
# Every operation pins the installed script. Retirement with an exclusive
# lock cannot remove state while a detached recorder or rsync sender owns it.
sysopen my $root_lock, "$root/supervisor.pl", O_RDONLY|O_NOFOLLOW or die "open supervisor: $!";
if ($mode eq 'retire') {
    @ARGV == 0 or die "invalid retirement request";
    if (!flock($root_lock, LOCK_EX | LOCK_NB)) {
        ($! == EAGAIN || $! == EWOULDBLOCK) or die "lock supervisor: $!";
        print "MARATHON_D96_1 busy\n";
        exit 0;
    }
    opendir my $dir, $root or die "open root: $!";
    my @files = grep { $_ ne '.' && $_ ne '..' && $_ ne 'supervisor.pl' } readdir $dir;
    closedir $dir or die "close root: $!";
    !grep { !/\A[a-f0-9]{32}\.(?:state|lock|pending|receipt|stderr|wake)\z/ } @files or die "unexpected root entries";
    unlink(map { "$root/$_" } @files) == @files or die "remove token state: $!";
    unlink "$root/supervisor.pl" or die "remove supervisor: $!";
    rmdir $root or die "remove root: $!";
    print "MARATHON_D96_1 retired\n";
    exit 0;
}
flock($root_lock, LOCK_SH | LOCK_NB) or die "supervisor is retiring: $!";
# A process that opened the old script before retirement might acquire its
# shared lock after the retire process exits. Require the installation to exist.
-f "$root/supervisor.pl" or die "supervisor retired";
if ($mode eq 'control-stream') {
    @ARGV == 0 or die "invalid control stream request";
    control_stream($root, $lease, $root_lock);
    exit 0;
}
if ($mode eq 'control') {
    for my $item (@ARGV) {
        $item =~ /\A([a-f0-9]{32}):(renew-[0-9]{1,16}|status|[1-3])\z/ or die "invalid request";
        my ($token, $op) = ($1, $2);
        my ($state) = control("$root/$token", $op, $lease);
        print "$token $state\n";
    }
    exit 0;
}
if ($mode eq 'rsync') {
    my ($token, $program) = splice @ARGV, 0, 2;
    $token =~ /\A[a-f0-9]{32}\z/ && $program =~ /\A[A-Za-z0-9+\/=]+\z/
        && @ARGV >= 2 && $ARGV[0] eq '--server' && $ARGV[1] eq '--sender' or die "invalid rsync sender";
    require MIME::Base64;
    exit_owned("$root/$token", $lease, undef, '/bin/sh', '-c', MIME::Base64::decode_base64($program) . ' "$@"', 'rsync', @ARGV);
}
if ($mode eq 'detach') {
    my $token = shift @ARGV;
    $token =~ /\A[a-f0-9]{32}\z/ && @ARGV or die "invalid detached command";
    pipe(my $reply, my $announce) or die "detach pipe: $!";
    my $supervisor = fork();
    defined($supervisor) or die "fork supervisor: $!";
    if ($supervisor) {
        close $announce;
        my $started;
        exit(sysread($reply, $started, 1) == 1 && $started eq 'G' ? 0 : 125);
    }
    close $reply;
    require POSIX;
    POSIX::setsid() > 0 or die "setsid: $!";
    open STDIN, '<', '/dev/null' or die "detach stdin: $!";
    open STDOUT, '>', '/dev/null' or die "detach stdout: $!";
    open STDERR, '>&', STDOUT or die "detach stderr: $!";
    exit_owned("$root/$token", $lease, $announce, @ARGV);
}
if ($mode eq 'run') {
    my $token = shift @ARGV;
    $token =~ /\A[a-f0-9]{32}\z/ && @ARGV or die "invalid run";
    exit_owned("$root/$token", $lease, undef, @ARGV);
}
die "invalid mode";
