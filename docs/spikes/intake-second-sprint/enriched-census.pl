# Census of enriched notes: reads TSV "path<TAB>via<TAB>enriched" (vault-relative path) and a vault root.
# Prints aggregates plus (to STDERR-free file arg 3, optional) the anonymous candidate list "row<TAB>path" for notes >=300 words with neither abstract nor removed block.
use strict;
my ($root,$tsv,$candout)=@ARGV; my (%tab,$n,$fold,$lines,$cand);
my @c;
open my $t,"<",$tsv or die;
while(<$t>){ chomp; my($p,$via,$enr)=split /\t/; next if $enr eq ""; 
  open my $h,"<","$root/$p" or next; local $/; my $x=<$h>; close $h; local $/="\n";
  my $fm=""; $fm=$1 if $x=~s/\A(---\n.*?\n---\n)//s;
  my $body=$x; my $w=()=$body=~/\S+/g;
  my $kind = $via=~/email/ ? "mail" : $via eq "" ? "other" : $via;
  my $abs = $body=~/^> \[!abstract\]-/m ? 1:0; my $rem = $body=~/^> \[!removed\]-/m ? 1:0;
  my $len = $w>=300 ? "300+" : $w>=150 ? "150-299" : "<150";
  $n++; $tab{"$kind\t$len\tabstract=$abs\tfold=$rem"}++;
  if($rem){ $fold++; my $in=0; for(split /\n/,$body){ if(/^> \[!removed\]-/){$in=1;next} if($in){ if(/^>/){$lines++ if /^> \S/}else{$in=0} } } }
  if($w>=300 && !$abs && !$rem){ $cand++; push @c,[$p,$kind,$w]; }
}
print "enriched notes: $n\nnotes with fold: $fold\nfold quoted lines (non-blank quoted lines): $lines\n300+ words, no abstract, no fold: $cand\n";
print "$tab{$_}\t$_\n" for sort keys %tab;
if($candout){ open my $o,">",$candout; my $i=0; print $o join("\t",++$i,@$_),"\n" for @c; }
