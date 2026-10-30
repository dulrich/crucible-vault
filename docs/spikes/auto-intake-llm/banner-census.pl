#!/usr/bin/perl
# banner-census.pl <vault> <days> — recurring images across tracker/clip notes (post-id or source
# frontmatter). Counts only: notes, image refs, refs whose target recurs in >=3 notes (by target and
# by content md5 for local files), and the position (first/last 15% of body) of recurring refs.
use strict; use warnings; use File::Find; use Digest::MD5;
my ($root,$days)=@ARGV; my $cut=time-$days*86400; my (%notes_by_t,%pos,%md5of); my ($nn,$refs)=(0,0);
find({no_chdir=>1, wanted=>sub{
  return if $File::Find::name =~ m{/\.(trash|git|obsidian)/|/_custom/}; return unless /\.md$/;
  my @st=stat($_); return unless @st && $st[9]>=$cut;
  open my $f,'<',$_ or return; local $/; my $t=<$f>; close $f;
  return unless $t=~s/\A---\n(.*?)\n---\n//s; my $fm=$1; return unless $fm=~/^source:/m;
  $nn++; my $dir=$File::Find::dir; my $len=length($t)||1; my %seen;
  while($t=~/!\[[^\]]*\]\(<?([^)\s>]+)>?[^)]*\)|!\[\[([^\]|#]+)/g){
    my $tg=$1//$2; my $p=pos($t)/$len; $refs++;
    my $key=$tg;
    if($tg!~m{^https?://}){ my $fp = -e "$dir/$tg" ? "$dir/$tg" : (-e "$root/$tg" ? "$root/$tg" : undef);
      if($fp){ $key = $md5of{$fp} //= do { open my $h,'<:raw',$fp; Digest::MD5->new->addfile($h)->hexdigest } } }
    else { $key=~s/[?#].*//; }
    next if $seen{$key}++; $notes_by_t{$key}++; push @{$pos{$key}}, $p;
  }
}}, $root);
my @rec = grep { $notes_by_t{$_}>=3 } keys %notes_by_t; my ($rr,$edge)=(0,0);
for(@rec){ for my $p (@{$pos{$_}}){ $rr++; $edge++ if $p<0.15 || $p>0.85 } }
my $top = join ',', (sort {$b<=>$a} map {$notes_by_t{$_}} @rec)[0..9];
print "notes=$nn image_refs=$refs distinct_targets=".scalar(keys %notes_by_t)." recurring_targets(>=3 notes)=".scalar(@rec)." refs_to_recurring=$rr of_which_in_first/last_15pct=$edge top10_note_counts=$top\n";
