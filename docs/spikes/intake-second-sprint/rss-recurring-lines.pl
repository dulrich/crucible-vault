use strict; my (%pubnotes);
while(<STDIN>){ chomp; my($p,$pub,$via,$enr)=split /\t/; $pub=~s/^[\["\s]+|[\]"\s]+$//g; next if $pub eq "" or $via=~/email/;
  open my $h,"<",$p or next; local $/; my $t=<$h>; close $h;
  $t=~s/\A---\n.*?\n---\n//s; $t=~s/%% cv-enrich %%.*?%% \/cv-enrich %%//sg;
  my @l = grep { length($_)>3 } map { my $x=lc; $x=~s/\s+/ /g; $x=~s/^ | $//g; $x } split /\n/, $t;
  next if @l<20;
  my @tail = @l[-12..-1]; my @head=@l[0..5];
  push @{$pubnotes{$pub}}, {tail=>\@tail, head=>\@head, enr=>($enr ne ""), n=>scalar(@l)}; }
my ($pubs,$pubsT,$pubsH,$notes,$nT,$nH,$lT,$lH,$enrN,$enrT,$enrLT,$big)=(0)x12;
for my $pub (keys %pubnotes){ my @n=@{$pubnotes{$pub}}; next if @n<5; $pubs++; $notes+=@n;
  my $thr = @n*0.3; $thr=3 if $thr<3; my (%ct,%ch);
  for my $x (@n){ my %s; $s{$_}=1 for @{$x->{tail}}; $ct{$_}++ for keys %s; my %g; $g{$_}=1 for @{$x->{head}}; $ch{$_}++ for keys %g; }
  my ($anyT,$anyH)=(0,0);
  for my $x (@n){ my $t = grep { $ct{$_}>=$thr } @{$x->{tail}}; my $hh = grep { $ch{$_}>=$thr } @{$x->{head}};
    if($t){$nT++;$lT+=$t;$anyT=1; $big++ if $t>=3} if($hh){$nH++;$lH+=$hh;$anyH=1}
    if($x->{enr}){ $enrN++; if($t){$enrT++; $enrLT+=$t} } }
  $pubsT+=$anyT; $pubsH+=$anyH; }
print "publications with >=5 RSS notes: $pubs ($notes notes)\n";
print "tail: $pubsT publications, $nT notes, $lT recurring lines ($big notes with >=3)\n";
print "head: $pubsH publications, $nH notes, $lH recurring lines\n";
print "enriched RSS notes in those pubs: $enrN; still carrying recurring tail lines in body after enrich: $enrT notes, $enrLT lines\n";
