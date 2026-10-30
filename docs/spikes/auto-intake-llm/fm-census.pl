#!/usr/bin/perl
# Frontmatter census over notes modified in the last N days: per `type` (or tag-derived kind),
# counts of notes with author/publication/source present+nonempty. Aggregates only; prints no values.
use strict; use warnings; use File::Find;
my ($root,$days)=@ARGV; my $cut=time-$days*86400; my %c;
find({no_chdir=>1, wanted=>sub{
  return if $File::Find::name =~ m{/\.(trash|git|obsidian)/|/_custom/}; return unless /\.md$/;
  my @st=stat($_); return unless @st; return if $st[9]<$cut;
  open my $f,'<',$_ or return; my $l=<$f>; return unless defined $l && $l=~/^---\s*$/;
  my %fm; my $lk; while(my $x=<$f>){ last if $x=~/^---\s*$/; if(defined $lk && $x=~/^\s+-\s*\S/){ $fm{$lk}="list"; next; } if($x=~/^([A-Za-z0-9_-]+):\s*(.*?)\s*$/){ my($k,$v)=($1,$2); $v='' if $v=~/^(""|''|\[\]|null|~)$/; $fm{$k}=$v; $lk=$k; } }
  close $f; return unless exists $fm{source};
  my $t = $fm{type} // ($fm{'post-id'} ? 'tracker' : 'other'); $t=~s/["'\[\]]//g; $t='(blank)' if $t eq '';
  my $s = $fm{source}; my $yt = ($s=~/youtu\.?be/)?'yt':'web';
  my $key="$t/$yt"; $c{$key}{n}++;
  for my $k (qw(author publication channel)){ $c{$key}{$k}++ if defined $fm{$k} && $fm{$k} ne ''; }
}}, $root);
printf "%-28s %5s %7s %11s %7s\n",'type/src','n','author','publication','channel';
printf "%-28s %5d %7d %11d %7d\n",$_,$c{$_}{n},$c{$_}{author}//0,$c{$_}{publication}//0,$c{$_}{channel}//0 for sort {$c{$b}{n}<=>$c{$a}{n}} keys %c;
