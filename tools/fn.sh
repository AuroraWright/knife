#!/bin/sh
# print decompiled function by name (without the local variable declarations)
awk -v n="$1" '/^\/\/ ==== /{p=($3==n)} p' /Users/aurora/tsukihime_reimpl/re/decomp/all.c | grep -vE '^  [A-Za-z_0-9 *]+ v[0-9]+(\[[0-9]+\])?; //'
