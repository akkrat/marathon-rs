use strict;
use warnings;
use POSIX qw(setpgid _exit);
use Time::HiRes qw(clock_gettime CLOCK_MONOTONIC);
use Fcntl qw(:DEFAULT :flock);
use Errno qw(ESRCH EAGAIN EWOULDBLOCK);
use File::Temp qw(tempdir);
use IO::Select;
use MIME::Base64 qw(decode_base64);

my ($encoded) = @ARGV;
defined($encoded) && $encoded =~ m{\A[A-Za-z0-9+/=]+\z} or die "invalid supervisor bytes";
defined(clock_gettime(CLOCK_MONOTONIC)) or die "monotonic clock unavailable";
my $root = tempdir('marathon-supervisor-XXXXXXXXXX', DIR => '/tmp', CLEANUP => 0);
my $path = "$root/supervisor.pl";
my $installed = eval {
    sysopen my $script, $path, O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW, 0600
        or die "create supervisor: $!";
    print {$script} decode_base64($encoded) or die "write supervisor: $!";
    close $script or die "close supervisor: $!";
    1;
};
if (!$installed) {
    my $error = $@;
    unlink $path;
    rmdir $root;
    die $error;
}
print "MARATHON_D96_1 $root\n";
