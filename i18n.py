"""Translations, through GNU gettext.

Strings shown to people are wrapped in `_()`; `xgettext` collects them into
po/almanac.pot, translators produce po/<language>.po, and the package build
compiles those to the system locale directory. A checkout run from source
uses ./locale instead, if `po/update.sh` has compiled it there.

With no translation for the user's language, `_()` returns the English text
unchanged -- nothing here can stop the app from starting.
"""
from __future__ import annotations

import gettext
import locale
from pathlib import Path

DOMAIN = "almanac"


def localedir() -> str | None:
    """./locale in a checkout; None means the system's own locale directory."""
    here = Path(__file__).resolve().parent / "locale"
    return str(here) if here.is_dir() else None


try:
    locale.setlocale(locale.LC_ALL, "")
except locale.Error:
    pass                                # an unsupported locale is still English

_translation = gettext.translation(DOMAIN, localedir=localedir(), fallback=True)
_ = _translation.gettext
ngettext = _translation.ngettext
