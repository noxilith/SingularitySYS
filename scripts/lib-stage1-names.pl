# Used by build-glibc-world.sh with `perl -0pi`: while the toolchain is the stage-1 bootstrap, a recipe's tool
# dependencies (named by their final names) are mapped to the bootstrap packages. Only quoted names inside the
# [depends] section are touched.
s{(\[depends\].*?)(?=\n\[|\z)}{
    my $d = $1;
    my %m = (
        busybox        => "busybox-stage0",
        make           => "make-stage0",
        binutils       => "binutils-glibc-stage1",
        gcc            => "gcc-glibc-stage1",
        glibc          => "glibc-stage1",
        "linux-headers" => "linux-headers-stage0",
    );
    $d =~ s/"([a-z0-9+_.-]+)"/exists $m{$1} ? "\"$m{$1}\"" : "\"$1\""/ge;
    $d;
}se;
