use strict; my (@recs,$cur);
open my $h,"<",$ARGV[0] or die;
while(<$h>){ if(/^\[\[publication\]\]/){$cur={};push @recs,$cur;next}
  if(/^\[/){undef $cur; next} next unless $cur;
  if(/^(name|short|kind)\s*=\s*"(.*)"/){$cur->{$1}=$2}
  if(/^(prefix)\s*=\s*\[(.*)\]/){ my($k,$v)=($1,$2); $cur->{$k}=[ $v=~/"([^"]+)"/g ] } }
my %names = map { (lc($_->{short}//$_->{name}) => 1) } @recs;
my (%t,%per);
open my $t,"<",$ARGV[1] or die;
while(<$t>){ chomp; my($src,$pub)=split /\t/; $src=~s/^"|"$//g; $pub=~s/^[\["\s]+|[\]"\s]+$//g;
  for my $i (0..$#recs){ my $r=$recs[$i]; my $want=$r->{short}//$r->{name};
    next unless grep { index($src,$_)==0 } @{$r->{prefix}||[]};
    last if $pub eq "" or $pub eq $want;
    my $c = lc($pub) eq lc($want) ? "case-only" : $pub eq $r->{name} ? "is-record-name-not-short" : $names{lc $pub} ? "another-configured-publication" : (index(lc $want, lc $pub)>=0 || index(lc $pub, lc $want)>=0) ? "substring-variant" : "unconfigured-value";
    $t{"$c kind=$r->{kind}"}++; $per{$i}{$pub}++; last } }
print "$t{$_}\t$_\n" for sort keys %t;
print "distinct other-values per record: ".join(",", map { scalar keys %{$per{$_}} } sort keys %per)."\n";
print "notes per record: ".join(",", map { my $s=0; $s+=$_ for values %{$per{$_}}; $s } sort keys %per)."\n";
