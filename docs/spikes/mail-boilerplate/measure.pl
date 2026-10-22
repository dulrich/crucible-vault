#!/usr/bin/perl
# Read-only: per-sender repeated-line boilerplate in `via: email` notes. Prints aggregates only.
use strict; use warnings;
my $root = shift or die "usage: measure.pl <vault>\n";
local $/ = "\0";
my (%notes_by, %lines_of);
open(my $f, "-|", "grep", "-rlZ", "--include=*.md", "^via: email", $root) or die;
while (my $p = <$f>) { chomp $p;
  open(my $h, "<", $p) or next; local $/; my $t = <$h>; close $h;
  next unless $t =~ /\A---\n(.*?)\n---\n(.*)\z/s; my ($fm,$body)=($1,$2);
  next unless $fm =~ /^via: email$/m;
  my ($pub) = $fm =~ /^publication:\s*"?([^"\n]*)/m; $pub //= "(none)";
  my @l = grep { /\S/ } map { s/\s+$//r } split /\n/, $body;
  push @{$notes_by{$pub}}, $p; $lines_of{$p} = \@l;
}
my ($N,$tot,$rep,$senders,$multi)=(0,0,0,0,0); my @per;
for my $pub (keys %notes_by) { $senders++; my @ps=@{$notes_by{$pub}}; $N+=@ps;
  my %df; for my $p (@ps){ my %s; $s{$_}=1 for @{$lines_of{$p}}; $df{$_}++ for keys %s; }
  my ($t,$r)=(0,0);
  for my $p (@ps){ for (@{$lines_of{$p}}){ $t++; $r++ if @ps>=2 && $df{$_}>=2 && length($_)>3; } }
  $tot+=$t; $rep+=$r; $multi++ if @ps>=2;
  push @per, sprintf("notes=%d lines=%d repeated=%d (%.0f%%)", scalar @ps, $t, $r, $t?100*$r/$t:0) if @ps>=2;
}
printf "email notes=%d senders=%d senders_with>=2=%d\nnonblank lines=%d repeated-within-sender=%d (%.1f%%)\n", $N,$senders,$multi,$tot,$rep,$tot?100*$rep/$tot:0;
print "per multi-note sender (anonymized):\n"; print "  $_\n" for sort @per;
