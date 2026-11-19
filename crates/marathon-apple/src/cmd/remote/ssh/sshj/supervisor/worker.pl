use strict;
use warnings;
use Time::HiRes qw(clock_gettime CLOCK_MONOTONIC);
use Fcntl qw(:DEFAULT :flock);
use Errno qw(ESRCH EAGAIN EWOULDBLOCK);

sub clock_ms { int(clock_gettime(CLOCK_MONOTONIC) * 1000) }

sub readable {
    my ($file, $delay) = @_;
    my $bits = '';
    vec($bits, fileno($file), 1) = 1;
    return select($bits, undef, undef, $delay) > 0;
}

sub read_state {
    my ($dir) = @_;
    return (0, 0, 0, '-') if !-e "$dir/state";
    open my $file, '<', "$dir/state" or die "read state: $!";
    my $state = <$file>;
    defined($state) && $state =~ /\A([01]) (\d+) ([0-3]) ([-a-z0-9:]+)\n\z/
        or die "invalid state";
    return ($1, $2, $3, $4);
}

sub write_state {
    my ($dir, @state) = @_;
    open my $file, '>', "$dir/pending" or die "write state: $!";
    print {$file} join(' ', @state), "\n" or die "write state: $!";
    close $file or die "close state: $!";
    rename "$dir/pending", "$dir/state" or die "commit state: $!";
}

sub locked {
    my ($dir, $op) = @_;
    mkdir $dir, 0700 unless -d $dir;
    sysopen my $lock, "$dir/lock", O_RDWR|O_CREAT|O_NOFOLLOW, 0600 or die "open lock: $!";
    if (!flock($lock, LOCK_EX | LOCK_NB)) {
        return undef if $! == EAGAIN || $! == EWOULDBLOCK;
        die "lock token: $!";
    }
    my ($new, $result) = $op->(read_state($dir));
    write_state($dir, @$new) if defined $new;
    close $lock or die "close lock: $!";
    return $result;
}

sub control {
    my ($dir, $op, $lease) = @_;
    return locked($dir, sub {
        my ($started, $expiry, $signal, $receipt) = @_;
        my $now = clock_ms();
        return (undef, $receipt) if $receipt ne '-';
        if ($op =~ /\A[1-3]\z/) {
            return ([0, 0, $op, 'not-started'], 'not-started') if !$started;
            $signal = $op if $op > $signal;
        } elsif ($op eq 'status') {
            return (undef, $started ? "running:$expiry" : 'unknown');
        } elsif ($op =~ /\Arenew-([0-9]+)\z/ && $started && $expiry == $1 && $expiry > $now && $signal < 2) {
            # Retries must carry the same stamp. At most one queued renewal can
            # extend an expiry the controller actually observed.
            my $proposed = $now + $lease;
            $expiry = $proposed if $proposed > $expiry;
        }
        return ([$started, $expiry, $signal, '-'], $started ? "running:$expiry" : 'unknown');
    }) // 'busy';
}

sub run_owned {
    my ($dir, $lease, $announce, @argv) = @_;
    my $start;
    my $admission_deadline = clock_ms() + $lease;
    while (!defined $start && clock_ms() < $admission_deadline) {
        $start = locked($dir, sub {
            my ($started, $expiry, $signal, $receipt) = @_;
            return (undef, 0) if $started || $signal || $receipt ne '-';
            my $new_expiry = clock_ms() + $lease;
            return ([1, $new_expiry, 0, '-'], $new_expiry);
        });
        select undef, undef, undef, 0.01 if !defined $start;
    }
    return 125 unless $start;

    pipe(my $ready, my $ready_w) or die "ready pipe: $!";
    pipe(my $go, my $go_w) or die "start pipe: $!";
    pipe(my $status, my $status_w) or die "status pipe: $!";
    local $SIG{CHLD} = 'DEFAULT';
    local $SIG{PIPE} = 'IGNORE';
    local $SIG{HUP} = 'IGNORE';
    local $SIG{INT} = 'IGNORE';
    local $SIG{TERM} = 'IGNORE';
    my $anchor = fork();
    defined($anchor) or die "fork anchor: $!";
    if ($anchor == 0) {
        close $announce if $announce;
        close $_ for ($ready, $go_w, $status);
        my $group_permit;
        sysread($go, $group_permit, 1) == 1 && $group_permit eq 'P' or exit(125);
        my $payload = fork();
        defined($payload) or exit(125);
        if ($payload == 0) {
            close $status_w;
            $SIG{$_} = 'DEFAULT' for qw(PIPE HUP INT TERM);
            syswrite($ready_w, 'R', 1) == 1 or exit(125);
            close $ready_w;
            my $permit;
            sysread($go, $permit, 1) == 1 && $permit eq 'G' or exit(125);
            close $go;
            exec { $argv[0] } @argv or exit(127);
        }
        close $_ for ($ready_w, $go);
        waitpid($payload, 0) == $payload or exit(125);
        syswrite($status_w, pack('N', $?), 4);
        close $status_w;
        # The supervisor sends group KILL before reaping this member.
        sleep 86400 while 1;
    }

    close $_ for ($ready_w, $go, $status_w);
    my $group = setpgrp($anchor, $anchor);
    my $permitted = $group && syswrite($go_w, 'P', 1) == 1;
    my $rc = 125;
    my $seen = 0;
    my ($expiry, $wanted) = ($start, 0);
    my $starting = 1;
    eval {
        while ($permitted) {
            my $observed = locked($dir, sub { (undef, [$_[1], $_[2]]) });
            ($expiry, $wanted) = @$observed if defined $observed;
            if (clock_ms() >= $expiry || $wanted == 3) {
                $rc = 137;
                last;
            }
            if ($starting && readable($ready, 0.025)) {
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
            if (!$starting && $wanted > $seen) {
                kill($wanted == 1 ? 'INT' : 'TERM', -$anchor);
                $seen = $wanted;
            }
            if (readable($status, 0.025)) {
                my $n = sysread($status, my $packet, 4);
                if ($n && $n == 4) {
                    my $wait_status = unpack('N', $packet);
                    $rc = ($wait_status & 127) ? 128 + ($wait_status & 127) : $wait_status >> 8;
                }
                last;
            }
        }
    };

    close $_ for ($ready, $status);
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
    my $receipt_deadline = clock_ms() + 5000;
    my $recorded;
    while (clock_ms() < $receipt_deadline) {
        $recorded = locked($dir, sub {
            return ([1, $_[1], $_[2], $reaped && $gone ? "done:$rc" : 'unconfirmed'], 1);
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
if ($mode eq 'control') {
    for my $item (@ARGV) {
        $item =~ /\A([a-f0-9]{32}):(renew-[0-9]{1,16}|status|[1-3])\z/ or die "invalid request";
        print "$1 ", control("$root/$1", $2, $lease), "\n";
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
