#!/usr/bin/env bash
# omatel is written in US English: strings, identifiers, comments, docs.
# This fails on common British spellings in tracked files. A line that
# must keep one (an old schema, another app's data) says "spelling: kept".
# GTK and GLib API names we don't own (Cancellable) are allowed.
set -euo pipefail
cd "$(dirname "$0")/.."
words='favour|colour|honour|behaviour|neighbour|flavour|harbour|rumour|labour|humour|centre|metre|litre|fibre|theatre|licence|defence|offence|catalogue|dialogue|\bprogramme\b|grey|whilst|amongst|cancell|labell|travell|modell|signall|counsell|totall|jewell|marvell|fuell|ageing|judgement|acknowledgement'
ise='(normali|organi|recogni|initiali|reali|finali|seriali|synchroni|customi|priori|summari|authori|minimi|maximi|optimi|apologi|utili|visuali|categori|saniti|standardi|memori|speciali|locali|capitali|tokeni|stabili|centrali|emphasi|criti)s(e|ed|es|ing|ation|ations)\b|analys(e|ed|es|ing)\b'
hits=$(git grep -nIiE "($words)|$ise" -- . ':!Cargo.lock' ':!scripts/spelling.sh' ':!LICENSE' \
  | grep -v 'spelling: kept' | grep -vE 'Cancellable' || true)
if [[ -n $hits ]]; then
  printf '%s\n' "$hits"
  echo "British spellings above: omatel uses US English" >&2
  exit 1
fi
echo "spelling: US English"
