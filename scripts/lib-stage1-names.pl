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
        "linux-headers" => "linux-headers-stage0",
    );
    # Once the real glibc exists the sandbox holds IT (USE_FINAL_GLIBC=1): the stage-1 compiler is the same
    # version and builds against it fine, and what is built then records the real glibc as its dependency.
    $m{glibc} = "glibc-stage1" unless $ENV{USE_FINAL_GLIBC};
    # Only the build list: what a package needs at run time is named by its final names, so the result never
    # depends on a bootstrap package.
    $d =~ s{^(build\s*=\s*\[[^\]]*\])}{
        my $line = $1;
        $line =~ s/"([a-z0-9+_.-]+)"/exists $m{$1} ? "\"$m{$1}\"" : "\"$1\""/ge;
        $line;
    }me;
    $d;
}se;
