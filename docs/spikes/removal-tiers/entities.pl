#!/usr/bin/perl
# Aggregates only: entities listed in existing `[!abstract]` top blocks — how many match an
# existing note basename, and how many recur across notes. Never prints names.
use strict; use warnings;
my $V = '~/vault';
my %base; for (split /\n/, `find $V -name '*.md' -not -path '*/.trash/*'`) { my ($b) = m{([^/]+)\.md$}; $base{lc $b} = 1 }
my (%in, $blocks, $ents, $with_ent, @sizes) = ();
for my $f (grep { !m{/\.trash/} } split /\n/, `grep -rl -F '> [!abstract]-' --include=*.md $V`) {
  open my $h, '<', $f or next; local $/; my $t = <$h>; close $h;
  my ($blk) = $t =~ /^%% cv-enrich %%\n(> \[!abstract\]-.*?)^%% \/cv-enrich %%/ms; next unless $blk; $blocks++;
  push @sizes, scalar(() = $blk =~ /\n/g);
  my ($line) = $blk =~ /^> Entities: (.*)$/m; next unless $line; $with_ent++;
  for (split /, (?=[^()]*(?:\(|$))/, $line) { s/ \([a-z]+\)$//; $ents++; $in{lc $_}{$f} = 1 }
}
my $distinct = keys %in; my $match = grep { $base{$_} } keys %in;
my %shared; for (keys %in) { my $c = keys %{$in{$_}}; $shared{$c >= 5 ? '5+' : $c}++ }
@sizes = sort { $a <=> $b } @sizes;
print "abstract blocks=$blocks with_entities=$with_ent median_lines=$sizes[$#sizes/2] max_lines=$sizes[-1]\n";
print "entity mentions=$ents distinct=$distinct  matching an existing note basename=$match\n";
print "distinct entities by number of notes listing them: ", join(' ', map { "$_:$shared{$_}" } sort keys %shared), "\n";
my $notes_with_shared = 0; my %nf; for my $e (keys %in) { next unless keys %{$in{$e}} >= 2; $nf{$_}++ for keys %{$in{$e}} }
printf "notes having >=1 entity shared with another note: %d of %d\n", scalar(keys %nf), $with_ent;
