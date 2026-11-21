#!/usr/bin/perl
# Aggregates only. Simulates the mail head/tail anchored strip (count>=2, MAX_GAP=2, half-body
# guard) over the current bodies of every `via: email` note, per publication, with exact line
# keys versus digit/weekday-masked keys. The learned set is approximated by the current bodies.
use strict; use warnings;
my $V = '~/vault';
my @files = grep { !m{/\.trash/} } split /\n/, `grep -rl -E '^via: "?email' --include=*.md $V`;
sub norm { my $l = shift; $l =~ s/\s+/ /g; $l =~ s/^ | $//g; $l }
sub mask { my $l = shift; $l =~ s/\d+/N/g; $l =~ s/\b(January|February|March|April|May|June|July|August|September|October|November|December|Jan|Feb|Mar|Apr|Jun|Jul|Aug|Sep|Sept|Oct|Nov|Dec|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday|Mon|Tue|Wed|Thu|Fri|Sat|Sun)\b/W/g; $l }
my %pub;
for my $f (@files) {
  open my $h, '<', $f or next; local $/; my $t = <$h>; close $h;
  my ($p) = $t =~ /^publication: "?([^"\n]+)"?$/m; next unless defined $p;
  $t =~ s/\A---\n.*?\n---\n//s;
  $t =~ s/^%% cv-enrich %%\n.*?^%% \/cv-enrich %%\n//msg;
  $t =~ s/^%% cv-enrich heading %%\n[^\n]*\n\n?//mg;
  push @{$pub{$p}}, [ map { norm($_) } split /\n/, $t ];
}
sub run { my ($k, $ord) = @_; my ($end, $gap) = (0, 0);
  for my $pos (0..$#$ord) { my $c = $k->[$ord->[$pos]];
    if ($c eq 'B') { $end = $pos + 1; $gap = 0 } elsif ($c eq 'C') { last if ++$gap > 2 } }
  $end }
sub sim { my ($notes, $key) = @_; my %cnt;
  for my $n (@$notes) { my %s; for (@$n) { next unless length > 3; my $k = $key->($_); $cnt{$k}++ unless $s{$k}++ } }
  my ($hit, $lines, $guard, $headhit) = (0, 0, 0, 0);
  for my $n (@$notes) { my @k = map { length($_) <= 3 ? 'N' : ($cnt{$key->($_)} >= 2 ? 'B' : 'C') } @$n;
    my $h = run(\@k, [0..$#k]); my $t = run(\@k, [reverse 0..$#k]);
    my %rm; $rm{$_} = 1 for 0..$h-1; $rm{$_} = 1 for ($#k-$t+1)..$#k;
    my $nb = grep { length } @$n; my $r = grep { length $n->[$_] } keys %rm;
    next unless $r; if ($r * 2 > $nb) { $guard++; next } $hit++; $lines += $r; $headhit++ if $h; }
  ($hit, $lines, $guard, $headhit) }
my (@tot_e, @tot_m); my ($pubs, $notes, $pubs_more) = (0, 0, 0);
for my $p (sort keys %pub) { my $ns = $pub{$p}; next if @$ns < 2; $pubs++; $notes += @$ns;
  my @e = sim($ns, sub { $_[0] }); my @m = sim($ns, \&mask);
  $tot_e[$_] += $e[$_], $tot_m[$_] += $m[$_] for 0..3; $pubs_more++ if $m[1] > $e[1];
  printf "pub n=%3d  exact: notes=%3d lines=%4d guard=%2d head=%3d | masked: notes=%3d lines=%4d guard=%2d head=%3d%s\n", scalar @$ns, @e, @m, ($p =~ /^Newsbit/ ? '  <- Example News AM' : '') if $m[1] != $e[1]; }
printf "TOTAL pubs=%d (changed=%d) notes=%d | exact: notes=%d lines=%d guard=%d head=%d | masked: notes=%d lines=%d guard=%d head=%d\n", $pubs, $pubs_more, $notes, @tot_e, @tot_m;
