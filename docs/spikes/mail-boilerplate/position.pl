#!/usr/bin/perl
# Read-only: where within-sender repeated lines sit (body position deciles) + max pairwise overlap per sender. Aggregates only.
use strict; use warnings;
my $root = shift or die "usage: position.pl <vault>\n";
local $/ = "\0"; my (%by,%L);
open(my $f, "-|", "grep", "-rlZ", "--include=*.md", "^via: email", $root) or die;
while (my $p=<$f>){ chomp $p; open(my $h,"<",$p) or next; local $/; my $t=<$h>; close $h;
  next unless $t =~ /\A---\n(.*?)\n---\n(.*)\z/s; my ($fm,$b)=($1,$2); next unless $fm=~/^via: email$/m;
  my ($pub)=$fm=~/^publication:\s*"?([^"\n]*)/m; $pub//="(none)";
  $L{$p}=[grep{/\S/}map{s/\s+$//r}split/\n/,$b]; push @{$by{$pub}},$p; }
my @dec=(0)x10; my ($edge,$mid,$contig)=(0,0,0); my @ov;
for my $pub (keys %by){ my @ps=@{$by{$pub}}; next if @ps<2;
  my %df; for my $p(@ps){my %s;$s{$_}=1 for @{$L{$p}};$df{$_}++ for keys %s}
  my $mx=0; for my $i(0..$#ps){for my $j($i+1..$#ps){my %a;$a{$_}=1 for @{$L{$ps[$i]}};my %b;$b{$_}=1 for @{$L{$ps[$j]}};
    my $in=grep{$b{$_}}keys %a; my $u=keys(%a)+keys(%b)-$in; my $o=$u?$in/$u:0; $mx=$o if $o>$mx}}
  push @ov, sprintf("%.2f",$mx);
  for my $p(@ps){ my @l=@{$L{$p}}; my $n=@l; next unless $n;
    # head run / tail run of repeated lines (tolerating 2 non-repeated gaps)
    my @r=map{ $df{$_}>=2 && length($_)>3 ?1:0 }@l;
    for my $i(0..$n-1){ next unless $r[$i]; $dec[int(10*$i/$n)]++; }
  } }
print "repeated-line position by body decile (0=top):\n  @dec\n";
print "max pairwise line-set Jaccard per multi-note sender:\n  @{[sort @ov]}\n";
