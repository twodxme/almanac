#!/bin/sh
# Refresh the translation template, merge it into every translation, and
# compile them into ./locale so a checkout run from source is translated too.
#   po/update.sh            from the repository root
# To start a new language:  msginit -i po/almanac.pot -o po/<lang>.po -l <lang>
set -eu
cd "$(dirname "$0")/.."
xgettext --language=Rust --keyword=tr --keyword=try_tr \
  --keyword=ngettext:1,2 \
  --from-code=UTF-8 --add-comments=TRANSLATORS: --sort-by-file \
  --package-name=almanac --msgid-bugs-address=https://github.com/twodxme/almanac/issues \
  -o po/almanac.pot $(find src -name '*.rs' | sort)
for po in po/*.po; do
  [ -e "$po" ] || continue
  lang=$(basename "$po" .po)
  msgmerge --quiet --update --backup=none "$po" po/almanac.pot
  mkdir -p "locale/$lang/LC_MESSAGES"
  msgfmt --check -o "locale/$lang/LC_MESSAGES/almanac.mo" "$po"
done
