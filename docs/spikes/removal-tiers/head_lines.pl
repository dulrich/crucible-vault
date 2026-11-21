#!/usr/bin/perl
# Aggregates only: the digit/weekday/url-masked lines that recur in the first N lines of one
# publication's notes. Prints counts and shapes, never note text.
use strict; use warnings;
my ($pub, $N) = (shift, shift // 8);
my @files = grep { !m{/\.trash/} } split /\n/, `grep -rl -E '^publication: "?\Q$pub\E' --include=*.md ~/vault`;
sub norm { my $l = shift; $l =~ s/\s+/ /g; $l =~ s/^ | $//g; $l }
sub mask { my $l = shift; $l =~ s/\((https?:[^)]*)\)/(U)/g; $l =~ s/https?:\S+/U/g; $l =~ s/\d+/N/g; $l =~ s/\b(January|February|March|April|May|June|July|August|September|October|November|December|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday)\b/W/gi; $l }
my (%cnt, %exact, %pos, %len, %flags, $with);
for my $f (@files) {
  open my $h, '<', $f or next; local $/; my $t = <$h>; close $h;
  $t =~ s/\A---\n.*?\n---\n//s;
  $t =~ s/^%% cv-enrich %%\n.*?^%% \/cv-enrich %%\n//msg;
  $t =~ s/^%% cv-enrich heading %%\n[^\n]*\n\n?//mg;
  my @l = grep { length($_) > 3 } map { norm($_) } split /\n/, $t;
  my %s; my $hit = 0;
  for my $p (0 .. ($#l < $N-1 ? $#l : $N-1)) {
    my $m = mask($l[$p]); next if $s{$m}++;
    $cnt{$m}++; $exact{$m}{$l[$p]} = 1; push @{$pos{$m}}, $p; $len{$m} = length $l[$p];
    $flags{$m} = join '', ($l[$p] =~ /!\[/ ? 'I' : '-'), ($l[$p] =~ /\]\(/ ? 'L' : '-'), ($l[$p] =~ /\d/ ? 'D' : '-'), ($l[$p] =~ /^#/ ? 'H' : '-'), ($l[$p] =~ /^>/ ? 'Q' : '-');
  }
}
my $n = @files; print "notes=$n  (flags: Image Link Digit Heading Quote)\n";
print "recurring masked head lines (>=5 notes):\n count  exact_variants  pos_min-max  len  flags\n";
for my $m (sort { $cnt{$b} <=> $cnt{$a} } grep { $cnt{$_} >= 5 } keys %cnt) {
  my @p = sort { $a <=> $b } @{$pos{$m}};
  printf " %5d  %14d  %5d-%-5d  %4d  %s\n", $cnt{$m}, scalar keys %{$exact{$m}}, $p[0], $p[-1], $len{$m}, $flags{$m};
}
