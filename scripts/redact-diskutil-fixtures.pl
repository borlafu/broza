#!/usr/bin/perl
#
# Redact recorded `diskutil` / `tmutil` output in place.
#
# Called by scripts/capture-diskutil-fixtures.sh with every captured file as an
# argument, in one invocation: the UUID map is shared across files so a container
# UUID keeps the same fake value in `list.plist` and in `apfs_list.plist`.
#
# Environment:
#   BROZA_SHORT_NAME      short user name to replace (`id -un`)
#   BROZA_FULL_NAME       full user name to replace (`id -F`)
#   BROZA_COMPUTER_NAME   computer name to replace (`scutil --get ComputerName`)

use strict;
use warnings;

# Replacements for the identity of the machine the fixtures came from.
use constant FAKE_SHORT_NAME    => 'testuser';
use constant FAKE_FULL_NAME     => 'Test User';
use constant FAKE_COMPUTER_NAME => 'test-mac';
# Per-user temporary directory hash, which is as identifying as a user name.
use constant FAKE_TEMP_DIR => '/private/var/folders/aa/bb00000000000000000000000000gn/';
# Value written over any serial number.
use constant FAKE_SERIAL => 'REDACTED-SERIAL';

my %uuid_map;
my $uuid_count = 0;

# Deterministic fake for one real UUID; equal inputs give equal outputs, and
# different inputs never collide.
sub fake_uuid {
    my ($real) = @_;
    return $uuid_map{$real} if exists $uuid_map{$real};
    $uuid_count += 1;
    my $fake = sprintf( '%08X-1111-4222-8333-%012X', $uuid_count, $uuid_count );
    $uuid_map{$real} = $fake;
    return $fake;
}

# Replace `$needle` wherever it appears, ignoring case; an empty needle is a
# no-op so a machine without a computer name cannot blank the whole file.
sub replace_literal {
    my ( $text, $needle, $replacement ) = @_;
    return $text if !defined $needle || $needle eq '';
    my $quoted = quotemeta $needle;
    $text =~ s/$quoted/$replacement/gi;
    return $text;
}

sub redact {
    my ($text) = @_;

    $text =~ s/([0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12})/fake_uuid($1)/ge;

    # `<key>…Serial…</key>` followed by its string value, on the next line.
    $text =~ s{(<key>[^<]*Serial[^<]*</key>\s*<string>)[^<]*(</string>)}
              {$1 . FAKE_SERIAL . $2}gse;

    $text =~ s{/private/var/folders/[^/<\s]+/[^/<\s]+/}{${\ FAKE_TEMP_DIR }}g;
    $text =~ s{(?<!private)/var/folders/[^/<\s]+/[^/<\s]+/}{${\ FAKE_TEMP_DIR }}g;

    $text = replace_literal( $text, $ENV{BROZA_FULL_NAME},     FAKE_FULL_NAME );
    $text = replace_literal( $text, $ENV{BROZA_COMPUTER_NAME}, FAKE_COMPUTER_NAME );
    $text = replace_literal( $text, $ENV{BROZA_SHORT_NAME},    FAKE_SHORT_NAME );

    return $text;
}

for my $path (@ARGV) {
    open my $in, '<', $path or die "cannot read $path: $!";
    local $/ = undef;
    my $text = <$in>;
    close $in;

    open my $out, '>', $path or die "cannot write $path: $!";
    print {$out} redact($text);
    close $out;
}
