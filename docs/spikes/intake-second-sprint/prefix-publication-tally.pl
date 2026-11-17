use strict; my (@recs,$cur);
open my $h,"<",$ARGV[0] or die;
while(<$h>){ if(/^\[\[publication\]\]/){$cur={};push @recs,$cur;next}
  if(/^\[/){undef $cur; next}
  next unless $cur;
  if(/^(name|short|kind)\s*=\s*"(.*)"/){$cur->{$1}=$2}
  if(/^(prefix|hosts)\s*=\s*\[(.*)\]/){ my($k,$v)=($1,$2); $cur->{$k}=[ $v=~/"([^"]+)"/g ] }
  elsif(/^(prefix|hosts)\s*=\s*"(.*)"/){ $cur->{$1}=[$2] }
}
my (%tot,%rec);
open my $t,"<",$ARGV[1] or die;
while(<$t>){ chomp; my($src,$pub,$day,$intake)=split /\t/; $src=~s/^"|"$//g; $pub=~s/^[\["\s]+|[\]"\s]+$//g;
  for my $i (0..$#recs){ my $r=$recs[$i]; my $want=$r->{short}//$r->{name};
    next unless grep { index($src,$_)==0 } @{$r->{prefix}||[]};
    my $c = $pub eq "" ? "blank" : $pub eq $want ? "same" : "other";
    my $w = $day ? ($intake ? "day-intake":"day-clip") : "elsewhere";
    $tot{"$c $w"}++; $rec{$c}{$i}++; last } }
print "$tot{$_}\t$_\n" for sort keys %tot;
print scalar(keys %{$rec{$_}})."\trecords with $_ notes\n" for sort keys %rec;
print scalar(grep {$_->{prefix}} @recs)." records have prefixes; multi-prefix: ".scalar(grep {@{$_->{prefix}||[]}>1} @recs)."\n";
