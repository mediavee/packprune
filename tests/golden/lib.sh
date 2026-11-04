# Shared by record and check: how a row runs and how its output is compared with the expected one.
# Sourced with the working directory at tests/golden. Bash 3.2 (macOS) compatible.

here=$(pwd)

# The absolute target directory of the workspace builds.
target_dir() {
    local target=${CARGO_TARGET_DIR:-target}
    case $target in /*) echo "$target" ;; *) echo "$(cd ../.. && pwd)/$target" ;; esac
}

# Sets `bin`: PACKPRUNE_BIN, or the workspace's debug build.
packprune_binary() {
    bin=${PACKPRUNE_BIN:-}
    if [ -z "$bin" ]; then
        bin=$(target_dir)/debug/packprune
        CARGO_TARGET_DIR=$(target_dir) cargo build --quiet --locked -p packprune-cli || return
    fi
    export bin
}

# Sets `audio`: GOLDEN_AUDIO_BIN, or the debug build of audio/.
audio_binary() {
    audio=${GOLDEN_AUDIO_BIN:-}
    if [ -z "$audio" ]; then
        audio=$(target_dir)/debug/golden-audio
        CARGO_TARGET_DIR=$(target_dir) cargo build --quiet --locked --manifest-path audio/Cargo.toml || return
    fi
    export audio
}

# Rows of a rows file whose name matches one of the patterns (all without), one per line:
#   <row> TAB <pack> TAB <configuration> TAB <analysis options>
rows() {
    local file=$1
    shift
    perl -e '
        my ($file, @patterns) = @ARGV;
        open my $in, "<", $file or die "$file: $!";
        while (<$in>) {
            next if /^\s*(#|$)/;
            my ($label, $pack, $configurations, @options) = split " ";
            for my $configuration (split /,/, $configurations) {
                my $row = "$label.$configuration";
                next if @patterns && !grep { my $re = quotemeta($_) =~ s/\\\*/.*/gr; $row =~ /^$re$/ } @patterns;
                print join("\t", $row, $pack, $configuration, join(" ", @options)), "\n";
            }
        }
    ' "$file" "$@"
}

# The words of `configuration` in the configurations file, one per line: its markers, then its options.
configuration() {
    perl -e '
        my $name = shift;
        open my $in, "<", "configurations" or die "configurations: $!";
        while (<$in>) {
            next if /^\s*(#|$)/;
            my ($first, @rest) = split " ";
            next unless $first eq $name;
            print "$_\n" for @rest;
            exit;
        }
        die "no configuration $name\n";
    ' "$1"
}

# The optimise options of `configuration`, without its markers.
options() {
    configuration "$1" | grep -v -x -E 'drop-in|transcode'
}

is_drop_in() {
    configuration "$1" | grep -q -x drop-in
}

# Whether a configuration transcodes sounds, so its rows compare sounds by `audio_check`.
is_transcode() {
    configuration "$1" | grep -q -x transcode
}

# The fixtures' client, zipped into a client jar (scripts/determinism does the same).
client_jar() {
    (cd ../fixtures/vanilla && perl -MIO::Compress::Zip=zip,\$ZipError -MFile::Find -e '
        my @files;
        find({ wanted => sub { push @files, $File::Find::name =~ s{^\./}{}r if -f }, no_chdir => 1 }, ".");
        zip([ sort @files ] => $ARGV[0]) or die $ZipError;
    ' "$1")
}

# The files of a zip or a directory pack, one relative path per line, sorted.
entries() {
    if [ -d "$1" ]; then
        (cd "$1" && find . -type f | sed 's|^\./||' | LC_ALL=C sort)
    else
        # From the central directory: a `lean` zip leaves its local names empty.
        perl -e '
            open my $f, "<:raw", $ARGV[0] or die "$ARGV[0]: $!\n"; local $/; my $zip = <$f>;
            my $end = rindex($zip, "PK\x05\x06"); $end >= 0 or die "$ARGV[0]: no end of central directory\n";
            my ($count, $size, $at) = unpack "x10 v V V", substr($zip, $end, 22);
            for (1 .. $count) {
                my ($signature, $name, $extra, $comment) = unpack "V x24 v v v", substr($zip, $at, 34);
                $signature == 0x02014b50 or die "$ARGV[0]: bad central directory\n";
                my $path = substr($zip, $at + 46, $name);
                print "$path\n" unless $path =~ m{/$};
                $at += 46 + $name + $extra + $comment;
            }
        ' "$1" | LC_ALL=C sort
    fi
}

# The files only `a` holds, as `-<path>`, then those only `b` holds, as `+<path>`.
entries_diff() {
    LC_ALL=C comm -23 <(entries "$1") <(entries "$2") | sed 's/^/-/'
    LC_ALL=C comm -13 <(entries "$1") <(entries "$2") | sed 's/^/+/'
}

# A `lean` zip leaves its local names empty: unzip warns of each and takes the central directory's, which is right.
extract() {
    mkdir -p "$2" || return
    unzip -q "$1" -d "$2" 2>&1 |
        grep -v -e 'mismatching "local" filename' -e 'continuing with "central" filename version' >&2
    [ "${PIPESTATUS[0]}" -le 1 ]
}

# The two keys Nexo writes in the PackSquash settings it hands to the squash command.
squash_settings() {
    printf "pack_directory = '%s'\noutput_file_path = '%s'\n" "$1" "$2"
}

# The analysis options as config sections, for the drop-in, which takes no option.
config_sections() {
    perl -e '
        my (%one, %many);
        while (@ARGV) {
            my ($option, $value) = (shift, shift);
            if ($option eq "--source") { my ($id, $path) = split /=/, $value, 2; $id =~ /^(configs|datapacks)$/ ? push @{ $many{$id} }, $path : ($one{$id} = $path) }
            elsif ($option eq "--keep") { $one{keep} = $value }
            elsif ($option eq "--prune-unused") { push @{ $many{prune_unused} }, $value }
            elsif ($option eq "--asset-index") { $one{asset_index} = $value }
            else { die "drop-in: no config key for $option\n" }
        }
        my $quote = sub { my $s = shift; $s =~ s/(["\\])/\\$1/g; "\"$s\"" };
        print "asset_index = ", $quote->(delete $one{asset_index}), "\n" if exists $one{asset_index};
        print "\n[sources]\n";
        print "$_ = ", $quote->($one{$_}), "\n" for sort keys %one;
        print "$_ = [", join(", ", map { $quote->($_) } @{ $many{$_} }), "]\n" for sort keys %many;
    ' -- "$@"
}

# Absolute form of each path in analysis options, for a run whose working directory is not this one.
absolute_options() {
    perl -MCwd=abs_path -e '
        while (@ARGV) {
            my ($option, $value) = (shift, shift);
            if ($option eq "--prune-unused") { print "$option\n$value\n"; next }
            my ($prefix, $path) = $option eq "--source" ? $value =~ /^([^=]+=)(.*)$/ : ("", $value);
            print "$option\n$prefix", abs_path($path) // die("$path: no such file\n"), "\n";
        }
    ' -- "$@"
}

# packprune on one row: `optimise`, or for `drop-in` the squash command `packprune <settings.toml>`. Writes `out`,
# its log to `work`/run.log; returns packprune's exit code.
#   run <binary> <client jar> <pack> <configuration> <out> <work> [analysis options...]
run() {
    local bin=$1 client=$2 pack=$3 configuration=$4 out=$5 work=$6
    shift 6
    mkdir -p "$work/home"
    if ! is_drop_in "$configuration"; then
        local opts
        set -f
        opts=($(options "$configuration"))
        set +f
        HOME=$work/home PACKPRUNE_CACHE=$work/cache "$bin" optimise "$pack" -o "$out" \
            --client-jar "$client" ${opts[@]+"${opts[@]}"} "$@" > "$work/run.log" 2>&1
        return
    fi
    local squash=$work/squash
    mkdir -p "$squash"
    copy_pack "$pack" "$squash/pack"
    squash_settings "$squash/pack" "$squash/out.zip" > "$squash/settings.toml"
    {
        printf 'version = 2\n\n[vanilla]\nclient_jar = "%s"\n' "$client"
        local args
        args=$(absolute_options "$@") || return
        set -f
        config_sections $args
        set +f
    } > "$squash/packprune.toml"
    (cd "$squash" && HOME=$work/home PACKPRUNE_CONFIG=$squash/packprune.toml PACKPRUNE_CACHE=$work/cache \
        "$bin" "$squash/settings.toml" > "$work/run.log" 2>&1) &&
        mv "$squash/out.zip" "$out"
}

# A pack as the directory Nexo hands to the squash command.
copy_pack() {
    if [ -d "$1" ]; then
        cp -R "$1" "$2"
    else
        extract "$1" "$2"
    fi
}

# The oracle's differences between the expected output and this one, one per line; returns the oracle's exit code.
# With `transcode`, the sounds' `samples` digests are left out: Vorbis encodes differ from one platform to another,
# so `audio_check` compares those sounds instead.
#   oracle <binary> <expected> <output> <log> <configuration> [oracle options...]
oracle() {
    local bin=$1 before=$2 after=$3 log=$4 configuration=$5
    shift 5
    "$bin" diff "$before" "$after" "$@" > "$log" 2>&1
    local code=$?
    if is_transcode "$configuration"; then
        grep '^DIFFERS ' "$log" | grep -v -E '/samples: "[^"]*" -> "[^"]*"$'
    else
        grep '^DIFFERS ' "$log"
    fi
    return $code
}

# The decoded sounds of this output against the expected one, within the tolerances of `audio-tolerances`.
#   audio_check <golden-audio binary> <expected> <output> <input pack> <log>
audio_check() {
    local tolerances
    tolerances=$(grep -v '^#' "$here/audio-tolerances") || return 2
    # shellcheck disable=SC2086
    "$1" "$2" "$3" --input "$4" $tolerances > "$5" 2>&1
}

# `--asset-index` of analysis options, for the oracle.
asset_index_option() {
    while [ $# -gt 0 ]; do
        if [ "$1" = --asset-index ]; then
            printf -- '--asset-index\n%s\n' "$2"
        fi
        shift
    done
}
