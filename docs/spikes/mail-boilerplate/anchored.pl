#!/usr/bin/perl
# Read-only: lines an anchored head/tail strip of within-sender repeated lines (gap<=2) would remove,
# vs repeated lines left mid-body; excludes senders whose notes are near-duplicates (Jaccard>0.5). Aggregates only.
use strict; use warnings;
my $root = shift or die "usage: anchored.pl <vault>\n";
local $/ = "\0"; my (%by,%L);
open(my $f, "-|", "grep", "-rlZ", "--include=*.md", "^via: email", $root) or die;
while (my $p=<$f>){ chomp $p; open(my $h,"<",$p) or next; local $/; my $t=<$h>; close $h;
  next unless $t =~ /\A---\n(.*?)\n---\n(.*)\z/s; my ($fm,$b)=($1,$2); next unless $fm=~/^via: email$/m;
  my ($pub)=$fm=~/^publication:\s*"?([^"\n]*)/m; $pub//="(none)";
  $L{$p}=[grep{/\S/}map{s/\s+$//r}split/\n/,$b]; push @{$by{$pub}},$p; }
my ($tot,$head,$tail,$mid,$notes,$touched,$maxfrac)=(0,0,0,0,0,0,0);
for my $pub (keys %by){ my @ps=@{$by{$pub}}; next if @ps<2;
  my %df; for my $p(@ps){my %s;$s{$_}=1 for @{$L{$p}};$df{$_}++ for keys %s}
  my $dup=0; for my $i(0..$#ps){for my $j($i+1..$#ps){my %a;$a{$_}=1 for @{$L{$ps[$i]}};my %b;$b{$_}=1 for @{$L{$ps[$j]}};
    my $in=grep{$b{$_}}keys %a; my $u=keys(%a)+keys(%b)-$in; $dup=1 if $u && $in/$u>0.5}}
  next if $dup;
  for my $p(@ps){ my @l=@{$L{$p}}; my $n=@l; $notes++; $tot+=$n;
    my @r=map{ $df{$_}>=2 && length($_)>3 ?1:0 }@l;
    my ($h,$g)=(0,0); for my $i(0..$n-1){ if($r[$i]){$h=$i+1;$g=0}else{last if ++$g>2} }
    my ($t,$g2)=($n,0); for(my $i=$n-1;$i>=$h;$i--){ if($r[$i]){$t=$i;$g2=0}else{last if ++$g2>2} }
    my $hs=grep{$_}@r[0..$h-1]; my $ts=$t<$n?grep{$_}@r[$t..$n-1]:0; my $all=grep{$_}@r;
    $head+=$h; $tail+=$n-$t; $mid+=$all-$hs-$ts; $touched++ if $h||$t<$n;
    my $fr=$n?($h+$n-$t)/$n:0; $maxfrac=$fr if $fr>$maxfrac; } }
printf "notes=%d (non-dup multi-note senders) lines=%d\nhead-strip=%d tail-strip=%d (%.1f%% of lines) notes-touched=%d max-strip-fraction=%.0f%%\nrepeated lines left mid-body=%d\n",
 $notes,$tot,$head,$tail,$tot?100*($head+$tail)/$tot:0,$touched,100*$maxfrac,$mid;
