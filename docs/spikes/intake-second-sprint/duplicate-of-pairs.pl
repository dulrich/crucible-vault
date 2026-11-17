# Reads TSV "clip-path<TAB>duplicate-of value" (vault-relative paths; run from the vault root). Resolves the [[link]] by exact path or unique basename, prints anonymous rows.
use strict; my %by; for my $f (`find . -name '*.md' -not -path './.trash/*'`){ chomp $f; $f=~s{^\./}{}; (my $b=$f)=~s{.*/}{}; $b=~s/\.md$//; push @{$by{$b}},$f; }
sub rd { open my $h,"<",$_[0] or return; local $/; my $x=<$h>; $x=~s/\A---\n.*?\n---\n//s; $x }
my $i=0; print "row\tclip_in_Clippings\ttarget_words\tclip_words\ttarget_resolved\tclip_has_transcript_heading\ttarget_has_transcript_heading\n";
while(<STDIN>){ chomp; my($p,$v)=split /\t/; $v=~s/^"\[\[|\]\]"$//g; $v=~s/\|.*//; (my $b=$v)=~s{.*/}{}; my $t = -f "$v.md" ? "$v.md" : (@{$by{$b}||[]}==1 ? $by{$b}[0] : "");
  my $cb=rd($p); my $tb=$t ne "" ? rd($t) : ""; my $cw=()=$cb=~/\S+/g; my $tw=()=$tb=~/\S+/g;
  print join("\t",++$i, ($p=~m{^Clippings/}?"yes":"no"), $t ne ""?$tw:"unresolved", $cw, $t ne ""?"yes":"no", ($cb=~/^##+ Transcript/mi?"yes":"no"), ($tb=~/^##+ Transcript/mi?"yes":"no")),"\n"; }
