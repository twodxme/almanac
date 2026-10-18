"""Parity cases for i18n.py.

i18n.py itself is a thin layer over the standard library's gettext, so the
Rust port carries the parts of gettext it relies on (find, _expand_lang,
locale.normalize, GNUTranslations, c2py). Those are checked here against
Python's own, on hand-built .mo files in a temp dir and on the real
catalogs in /usr/share/locale (read only).
"""
import atexit
import importlib
import gettext
import locale
import os
import shutil
import struct
import tempfile

import i18n

_TMP = tempfile.mkdtemp(prefix="almanac-i18n-parity-")
atexit.register(shutil.rmtree, _TMP, True)


def _mo(entries, big_endian=False, version=0, magic=None, truncate=None, corrupt=False):
    """A .mo file from (msgid bytes, msgstr bytes) pairs, sorted as msgfmt does."""
    entries = sorted(entries)
    n = len(entries)
    e = ">" if big_endian else "<"
    head = 28
    ids_tbl = head
    strs_tbl = ids_tbl + 8 * n
    data_at = strs_tbl + 8 * n
    blob = b""
    ids, strs = [], []
    for mid, _ in entries:
        ids.append((len(mid), data_at + len(blob)))
        blob += mid + b"\0"
    for _, mstr in entries:
        strs.append((len(mstr), data_at + len(blob)))
        blob += mstr + b"\0"
    if corrupt and strs:
        strs[-1] = (strs[-1][0], 10**6)
    if magic is None:
        magic = 0x950412de
    out = struct.pack(e + "I", magic)
    out += struct.pack(e + "6I", version, n, ids_tbl, strs_tbl, 0, 0)
    for l, o in ids:
        out += struct.pack(e + "2I", l, o)
    for l, o in strs:
        out += struct.pack(e + "2I", l, o)
    out += blob
    if truncate is not None:
        out = out[:truncate]
    return out


def _write(name, lang, data, domain="almanac"):
    d = os.path.join(_TMP, name, lang, "LC_MESSAGES")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, domain + ".mo"), "wb") as f:
        f.write(data)
    return os.path.join(_TMP, name)


def _header(charset="UTF-8", plural=None, extra=""):
    h = "Project-Id-Version: almanac\nMIME-Version: 1.0\n"
    if charset is not None:
        h += f"Content-Type: text/plain; charset={charset}\n"
    if plural is not None:
        h += f"Plural-Forms: {plural}\n"
    return (h + extra).encode("utf-8")


def _py_t(domain, localedir, languages):
    return gettext.translation(domain, localedir=localedir, languages=languages, fallback=True)


def _tg(domain, localedir, languages, messages):
    args = {"domain": domain, "localedir": localedir, "languages": languages, "messages": messages}
    return "translation_gettext", args, lambda: [_py_t(domain, localedir, languages).gettext(m) for m in messages]


def _tn(domain, localedir, languages, messages, plural, n):
    args = {"domain": domain, "localedir": localedir, "languages": languages,
            "messages": messages, "plural": plural, "n": n}
    return "translation_ngettext", args, lambda: [_py_t(domain, localedir, languages).ngettext(m, plural, n) for m in messages]


TEXTS = ["History", "", "No string anyone has translated", "Ünïcødé ✓ 日本語 🎉",
         "with\nnewline", "%s apps", "a\x00b", "  padded  ", "\x04ctx"]

LOCALES = ["de", "en_US.UTF-8", "en_US.utf8", "pt_BR", "sr@latin", "C", "POSIX", "de_DE@euro",
           "fr_FR.ISO-8859-1@euro", "ca_ES@valencia", "zh_TW.big5", "", "xx_YY", "ENGLISH",
           "deutsch", "bokmål", "en_US.UTF-8@foo", "C.UTF-8", "ja_JP.eucJP", "uz_UZ.UTF-8@cyrillic",
           "be_BY@latin", "a:b", "sr_RS.utf8@latin", "nb_NO.ISO8859-1@euro", "de_DE.latin1",
           "de_DE.ISO_8859-15", "en_IE@euro", "it_IT.UTF-8@euro", "tt_RU@iqtelif", "ru_RU.koi8r",
           "es_ES.utf-8", "en@quot", "x.y.z", "@", ".", "_", "de_AT.cp1252@euro", "ÄÖ_Ü"]

PLURALS = [
    "n != 1", "(n != 1)", "n>1", "0", "n==1 ? 0 : 1",
    "n==1 ? 0 : n==2 ? 1 : 2",
    "(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2)",
    "(n==1 ? 0 : n%10>=2 && n%10<=4 && (n%100<12 || n%100>14) ? 1 : 2)",
    "(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5)",
    "(n%100==1 ? 0 : n%100==2 ? 1 : n%100==3 || n%100==4 ? 2 : 3)",
    "!n == 1", "!!n", "!(n == 1)", "1 < n < 5", "n == 1 == 1", "n < 2 == 1", "n > 1 ? n < 5 ? 1 : 2 : 0",
    "n ? 1 : 2 ? 3 : 4", "007", "2 - n - 1", "n*n*n%7", "n/2", "n/3*3 == n", "n || 5", "n && 7",
    "0 || n", "n % 10 + 1", "n+!n", "!n+1", "n/0", "n%0", "", "n +", "x", "n1", "12n", "n = 1",
    "(n", "n)", "n ? 1", "n == 1 &", "\tn\t!=\t1", "n\n", "(" * 21 + "n" + ")" * 21,
    "(" * 20 + "n" + ")" * 20, "n" + " " * 1000, "1 ? 2 : 3 ? 4 : 5", "n >= 2 <= 3",
    "n !== 1", "n<=1", "-1", "n - -1", "n%10==1 && n%100!=11", "ñ",
]
NS = list(range(0, 30)) + [100, 101, 102, 111, 112, 1000000, 1000001]


def _c2py(p):
    f = gettext.c2py(p)
    return [f(n) for n in NS]


def _system_messages(domain, lang, limit=25):
    """Real msgids from the machine's catalog, read through Python's gettext."""
    try:
        t = gettext.translation(domain, languages=[lang])
    except OSError:
        return [], []
    singles = [k for k in t._catalog if isinstance(k, str)][:limit]
    plurals = sorted({k[0] for k in t._catalog if isinstance(k, tuple)})[:limit]
    return singles, plurals


def cases():
    yield "DOMAIN", {}, lambda: i18n.DOMAIN
    yield "localedir", {}, lambda: i18n.localedir()
    for t in TEXTS:
        yield "_", {"text": t}, (lambda t=t: i18n._(t))
    for n in [0, 1, 2, 5, 21, -1, -5, 10**12, 2**40]:
        yield "ngettext", {"singular": "one app", "plural": "%d apps", "n": n}, (lambda n=n: i18n.ngettext("one app", "%d apps", n))
    yield "ngettext", {"singular": "", "plural": "", "n": 1}, lambda: i18n.ngettext("", "", 1)
    yield "ngettext", {"singular": "Ü✓", "plural": "日本", "n": 3}, lambda: i18n.ngettext("Ü✓", "日本", 3)

    for loc in LOCALES:
        yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
        yield "expand_lang", {"loc": loc}, (lambda loc=loc: gettext._expand_lang(loc))
    for p in PLURALS:
        yield "c2py", {"plural": p, "ns": NS}, (lambda p=p: _c2py(p))

    # gettext.find on the real machine: the environment as it is, then named languages
    langs = ["de", "fr", "pl", "ru", "ar", "ja", "sr@latin", "ca@valencia", "pt_BR", "zh_CN",
             "en_GB", "en@quot", "cs", "xx"]
    for dom in ["almanac", "coreutils", "glib20", "gtk40", "libadwaita", "nonexistent-domain"]:
        yield "find", {"domain": dom}, (lambda dom=dom: gettext.find(dom, all=True))
        yield "find", {"domain": dom, "languages": langs}, (lambda dom=dom: gettext.find(dom, languages=langs, all=True))
    for lang in ["de", "pl", "ru", "ar", "ja", "fr", "cs", "sr@latin", "en_GB", "en@quot"]:
        for dom in ["coreutils", "glib20", "gtk40", "libadwaita", "bash", "git"]:
            singles, plurals = _system_messages(dom, lang)
            if not singles and not plurals:
                continue
            yield _tg(dom, None, [lang], singles + plurals + ["Not in any catalog"])
            for n in [0, 1, 2, 3, 5, 11, 21, 22, 101, 1000]:
                yield _tn(dom, None, [lang], plurals + ["Not in any catalog"], "PLURAL", n)
    # several languages: the first catalog wins, the rest are fallbacks
    s, p = _system_messages("coreutils", "de")
    yield _tg("coreutils", None, ["xx", "pl", "de"], s + p)

    # hand-built catalogs
    good = [(b"", _header(plural="nplurals=3; plural=(n%10==1 && n%100!=11 ? 0 : n%10>=2 && n%10<=4 && (n%100<10 || n%100>=20) ? 1 : 2);")),
            (b"History", "Historia ✓".encode()), (b"Apps", b"Aplikacje"),
            (b"one app\0%d apps", "%d aplikacja\0%d aplikacje\0%d aplikacji".encode()),
            (b"ctx\x04Open", b"Otworz"), ("Ünï".encode(), "Ü".encode())]
    msgs = ["History", "Apps", "one app", "ctx\x04Open", "Ünï", "", "missing", "%d apps"]
    d = _write("good", "pl", _mo(good))
    yield _tg("almanac", d, ["pl"], msgs)
    for n in [0, 1, 2, 4, 5, 12, 22, 25, 101, 111]:
        yield _tn("almanac", d, ["pl"], ["one app", "missing", "History"], "%d apps", n)
    d = _write("bigendian", "pl", _mo(good, big_endian=True))
    yield _tg("almanac", d, ["pl"], msgs)
    yield _tn("almanac", d, ["pl"], ["one app"], "%d apps", 3)
    # chain: de has one string, fr another; languages=[de, fr]
    _write("chain", "de", _mo([(b"", _header()), (b"History", b"Verlauf")]))
    d = _write("chain", "fr", _mo([(b"", _header(plural="nplurals=2; plural=(n > 1);")),
                                    (b"History", b"Historique"), (b"Apps", b"Applications"),
                                    (b"one app\0%d apps", b"%d application\0%d applications")]))
    yield _tg("almanac", d, ["de", "fr"], msgs)
    yield _tg("almanac", d, ["fr", "de"], msgs)
    for n in [0, 1, 2]:
        yield _tn("almanac", d, ["de", "fr"], ["one app"], "%d apps", n)
    yield _tg("almanac", d, ["it"], msgs)
    yield _tg("almanac", d, [], msgs)
    yield _tg("almanac", d, ["de_DE.UTF-8"], msgs)
    yield _tg("almanac", d, ["C", "de"], msgs)
    # charsets
    d = _write("latin1", "de", _mo([(b"", _header(charset="ISO-8859-1")), (b"Size", "Größe".encode("latin-1"))]))
    yield _tg("almanac", d, ["de"], ["Size", ""])
    d = _write("ascii", "de", _mo([(b"", _header(charset=None)), (b"Size", b"Groesse")]))
    yield _tg("almanac", d, ["de"], ["Size", ""])
    d = _write("ascii-bad", "de", _mo([(b"", _header(charset=None)), (b"Size", "Größe".encode())]))
    yield _tg("almanac", d, ["de"], ["Size"])
    d = _write("utf8-bad", "de", _mo([(b"", _header()), (b"Size", b"Gr\xf6\xdfe")]))
    yield _tg("almanac", d, ["de"], ["Size"])
    d = _write("utf8-alias", "de", _mo([(b"", _header(charset="utf8")), (b"Size", "Größe".encode())]))
    yield _tg("almanac", d, ["de"], ["Size"])
    d = _write("no-charset-key", "de", _mo([(b"", b"Content-Type: text/plain\n"), (b"Size", b"x")]))
    yield _tg("almanac", d, ["de"], ["Size"])
    d = _write("empty-charset", "de", _mo([(b"", b"Content-Type: text/plain; charset=\n"), (b"Size", b"x")]))
    yield _tg("almanac", d, ["de"], ["Size", ""])
    d = _write("unknown-charset", "de", _mo([(b"", _header(charset="NOT-A-CHARSET")), (b"Size", b"x")]))
    yield _tg("almanac", d, ["de"], ["Size"])
    # header oddities: comments, continuation lines, bad Plural-Forms
    d = _write("header", "de", _mo([(b"", b"#-#-#-#-#  de.po  #-#-#-#-#\nProject-Id-Version: x\n  continued\n"
                                         b"Content-Type: text/plain; charset=UTF-8\n"
                                         b"Plural-Forms: nplurals=2; plural=n==1 ? 0 : 1;\n"),
                                    (b"one app\0%d apps", b"eine\0viele")]))
    for n in [0, 1, 2]:
        yield _tn("almanac", d, ["de"], ["one app"], "%d apps", n)
    yield _tg("almanac", d, ["de"], ["", "one app"])
    d = _write("plural-nosemi", "de", _mo([(b"", _header(plural="nplurals=2 plural=n!=1")), (b"A", b"B")]))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("plural-bad", "de", _mo([(b"", _header(plural="nplurals=2; plural=n+;")), (b"A", b"B")]))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("plural-div0", "de", _mo([(b"", _header(plural="nplurals=2; plural=n/0;")), (b"A", b"B"),
                                         (b"one\0many", b"x\0y")]))
    yield _tg("almanac", d, ["de"], ["A"])
    yield _tg("almanac", d, ["de"], ["missing"])
    yield _tn("almanac", d, ["de"], ["one"], "many", 1)
    d = _write("plural-only", "de", _mo([(b"", _header()), (b"file\0files", b"Datei\0Dateien")]))
    yield _tg("almanac", d, ["de"], ["file", "files"])
    yield _tn("almanac", d, ["de"], ["file"], "files", 7)
    d = _write("three-nul", "de", _mo([(b"", _header()), (b"a\0b\0c", b"x\0y\0z")]))
    yield _tg("almanac", d, ["de"], ["a"])
    # broken files
    d = _write("badmagic", "de", _mo([(b"", _header()), (b"A", b"B")], magic=0x12345678))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("badversion", "de", _mo([(b"", _header()), (b"A", b"B")], version=2 << 16))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("minorversion", "de", _mo([(b"", _header()), (b"A", b"B")], version=(1 << 16) | 7))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("corrupt", "de", _mo([(b"", _header()), (b"A", b"B")], corrupt=True))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("truncated", "de", _mo([(b"", _header()), (b"A", b"B")], truncate=40))
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("tiny", "de", b"\xde\x12\x04\x95\0\0")
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("empty", "de", b"")
    yield _tg("almanac", d, ["de"], ["A"])
    d = _write("nomessages", "de", _mo([]))
    yield _tg("almanac", d, ["de"], ["A", ""])
    yield "find", {"domain": "almanac", "localedir": d, "languages": ["de", "de_DE", "C", "de"]}, \
        (lambda d=d: gettext.find("almanac", localedir=d, languages=["de", "de_DE", "C", "de"], all=True))
    yield "find", {"domain": "almanac", "localedir": d, "languages": ["deutsch"]}, \
        (lambda d=d: gettext.find("almanac", localedir=d, languages=["deutsch"], all=True))
    yield "find", {"domain": "almanac", "localedir": os.path.join(_TMP, "missing"), "languages": ["de"]}, \
        (lambda: gettext.find("almanac", localedir=os.path.join(_TMP, "missing"), languages=["de"], all=True))

    # --- review round 1 (adversarial) ---
    # charsets Python's codecs decode but the port does not carry
    for i, (cs, raw) in enumerate([("ISO-8859-2", "Zażółć".encode("iso-8859-2")),
                                   ("ISO-8859-15", "Größe €".encode("iso-8859-15")),
                                   ("CP1251", "Размер".encode("cp1251")),
                                   ("KOI8-R", "Размер".encode("koi8-r")),
                                   ("cp1252", "Größe".encode("cp1252")),
                                   ("utf-8-sig", "Größe".encode("utf-8")),
                                   ("EUC-JP", "サイズ".encode("euc-jp"))]):
        d = _write(f"r1-charset-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size", ""])
    # header line wrapped in characters str.strip() removes but str::trim() keeps (U+001C..U+001F)
    d = _write("r1-strip", "de", _mo([(b"", b"\x1cContent-Type: text/plain; charset=ISO-8859-1\x1f\n"),
                                      (b"Size", "Größe".encode("latin-1"))]))
    yield _tg("almanac", d, ["de"], ["Size", ""])
    d = _write("r1-strip-plural", "de", _mo([(b"", _header() + b"\x1dPlural-Forms: nplurals=2; plural=n>5;\n"),
                                             (b"one\0many", b"x\0y")]))
    for n in [0, 1, 3, 7]:
        yield _tn("almanac", d, ["de"], ["one"], "many", n)
    # encodings.normalize_encoding: str.isalnum() vs char::is_alphanumeric (U+093E is Mc, Other_Alphabetic)
    for loc in ["de.fooाbar", "de_DE.fooाbar@euro", "fr.xͅy", "de.ाutf8"]:
        yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
        yield "expand_lang", {"loc": loc}, (lambda loc=loc: gettext._expand_lang(loc))
    # Python ints are unbounded; the port evaluates in i128
    for p, ns in [("100000000000000000000000000000000000000000", [0, 1]),
                  ("n % 100000000000000000000000000000000000000000", [0, 5]),
                  ("n*n*n%7", [2**62, 2**62 + 1]),
                  ("n*n*n*n > 1", [2**40])]:
        yield "c2py", {"plural": p, "ns": ns}, (lambda p=p, ns=ns: [gettext.c2py(p)(n) for n in ns])
    # deep but legal expressions
    for p in ["!" * 999 + "n", "n?n:" * 249 + "n"]:
        yield "c2py", {"plural": p, "ns": [0, 1, 2]}, (lambda p=p: [gettext.c2py(p)(n) for n in [0, 1, 2]])
    # a catalog whose plural only overflows i128
    d = _write("r1-bigplural", "de", _mo([(b"", _header(plural="nplurals=2; plural=n % 100000000000000000000000000000000000000000 != 1;")),
                                          (b"A", b"B"), (b"one\0many", b"eins\0viele")]))
    yield _tg("almanac", d, ["de"], ["A", "missing"])
    yield _tn("almanac", d, ["de"], ["one"], "many", 2)
    # gettext.find with the environment as it is (run also with LANGUAGE set to non-UTF-8 bytes)
    yield "find", {"domain": "coreutils"}, (lambda: gettext.find("coreutils", all=True))

    # --- review round 1 fixes ---
    # the same big values, exactly (JSON numbers beyond i64 are floats on the Rust side)
    for p, ns in [("100000000000000000000000000000000000000000", [0, 1]),
                  ("n % 100000000000000000000000000000000000000000", [0, 5, -5]),
                  ("n*n*n%7", [2**62, 2**62 + 1, -(2**62)]),
                  ("n*n*n*n", [2**40, -(2**40)]),
                  ("n*n*n*n/7 - n*n", [2**61]),
                  ("0 - n*n*n % 1000000000000000000000000000007", [2**50])]:
        yield "c2py_str", {"plural": p, "ns": ns}, (lambda p=p, ns=ns: [str(gettext.c2py(p)(n)) for n in ns])
    # lazy tokenizer and Python's \w / \b
    for p in ["n n $", "n $", "ń", "n²", "n_", "2²", "nͅ", "n ा", "n == 1 &", "n = = 1", "n | 1", "12n",
              "n ? 1 : $", "( $", "n<", "n<<1"]:
        yield "c2py", {"plural": p, "ns": [0, 1, 2]}, (lambda p=p: [gettext.c2py(p)(n) for n in [0, 1, 2]])
    # every charmap codec, every byte (undefined bytes left out, then one alone)
    import encodings
    for name in sorted({m[:-3] for m in os.listdir(os.path.dirname(encodings.__file__))
                        if m.endswith(".py") and not m.startswith("_")}):
        try:
            mod = importlib.import_module("encodings." + name)
            if not (hasattr(mod, "decoding_table") or hasattr(mod, "decoding_map")) or name == "charmap":
                continue
        except Exception:
            continue
        good, bad = b"", None
        for b in range(1, 256):
            try:
                bytes([b]).decode(name)
                if b != 10:
                    good += bytes([b])
            except UnicodeDecodeError:
                bad = bad or bytes([b])
        cs = name.replace("_", "-").upper()
        d = _write(f"r2-charmap-{name}", "xx", _mo([(b"", _header(charset=cs)), (b"All", good)]
                                                + ([(b"Bad", bad)] if bad else [])))
        yield _tg("almanac", d, ["xx"], ["All"])
    # multibyte charsets and the UTF-16/32 family
    for i, (cs, text) in enumerate([("Shift_JIS", "サイズ 日本語"), ("cp932", "サイズ"), ("GBK", "大小 中文"),
                                    ("GB2312", "大小"), ("GB18030", "大小 𠀀"), ("Big5", "大小 中文"),
                                    ("cp950", "大小"), ("EUC-KR", "크기 한국어"), ("cp949", "크기"),
                                    ("ISO-2022-JP", "サイズ"), ("UTF-16", "Größe"), ("UTF-16LE", "Größe 🎉"),
                                    ("UTF-16BE", "Größe"), ("UTF-32", "Größe"), ("UTF-32-BE", "Größe 🎉"),
                                    ("utf_8_sig", "﻿Größe"), ("latin9", "€uro"), ("windows-1251", "Привет"),
                                    ("IBM850", "Größe"), ("mac_roman", "Größe"), ("TIS-620", "ขนาด")]):
        raw = text.encode(cs)
        d = _write(f"r2-mb-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size"])
    # bytes Python's codec rejects
    for i, (cs, raw) in enumerate([("Shift_JIS", b"\x82"), ("EUC-JP", b"\xa4"), ("GBK", b"\x81"),
                                   ("Big5", b"\xa4"), ("EUC-KR", b"\xb0"), ("UTF-16", b"a"),
                                   ("UTF-16LE", b"\x00\xd8"), ("UTF-32", b"\x00\x00\x11\x00"),
                                   ("utf-8-sig", b"\xef\xbb"), ("cp1252", b"\x81"), ("utf-7", b"+AGEA-"),
                                   ("base64", b"YQ=="), ("undefined", b"a"), ("mbcs", b"a"), ("charmap", b"a"),
                                   ("unicode_escape", b"a"), ("latin-1\x00", b"a"), ("ISO 8859-2", "ż".encode("iso8859-2")),
                                   ("iso-8859-2 ", "ż".encode("iso8859-2")), ("L2", "ż".encode("iso8859-2")),
                                   ("aliases", b"a"), ("utf.8", b"a"), ("ütf-8", b"a")]):
        d = _write(f"r2-badbytes-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size"])
    # the module catalog loads (or not) exactly as `import i18n` does
    yield "init", {}, lambda: None
    for i, (cs, raw) in enumerate([("unicode_escape", rb"a\n\t\\ \x41\u00fc\U0001F389 \101\0 \q \
x"), ("unicode_escape", b"\xe9\\'"), ("unicode_escape", rb"\x4"), ("unicode_escape", b"a\\"),
                                   ("unicode_escape", rb"\U00110000"), ("raw_unicode_escape", rb"a\u00fc \\u00fc \\\u00fc \n \U0001F389"),
                                   ("raw_unicode_escape", rb"\u12"), ("raw-unicode-escape", b"\xe9\\")]):
        d = _write(f"r2-escape-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size"])

    # --- review round 2 (adversarial) ---
    # multibyte charsets: bytes where Python's strict codec and the Encoding
    # Standard decoder the port borrows disagree
    for i, (cs, raw) in enumerate([
            ("GB2312", b"\x81\x40"), ("GB2312", b"\xa1\xa4"), ("GBK", b"\x81\x30\x81\x30"), ("GBK", b"\x80"),
            ("Shift_JIS", b"\x87\x40"), ("Shift_JIS", b"\x80"), ("Shift_JIS", b"\xa0"), ("cp932", b"\x81\x60"),
            ("cp932", b"\xa0"), ("cp932", b"\xfd"), ("EUC-KR", b"\x81\x41"), ("Big5", b"\xa3\xe1"),
            ("Big5", b"\x87\x40"), ("Big5", b"\xf9\xf9"), ("cp950", b"\xf9\xf9"), ("big5hkscs", b"\x88\x62"),
            ("EUC-JP", b"\xa1\xc1"), ("EUC-JP", b"\xa1\xbd"), ("EUC-JP", b"\x8f\xa2\xb7"),
            ("ISO-2022-JP", b"\x1b(Ia\x1b(B"), ("GB18030", b"\x84\x31\xa4\x39")]):
        d = _write(f"r3-mb-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size"])
    # codecs Python has that the port does not carry at all
    for i, (cs, raw) in enumerate([("utf-7", b"+AGE-"), ("hz", b"~{<:~}"), ("punycode", b"gre-6ka8i"),
                                   ("johab", b"\x88\x61"), ("iso2022_kr", b"\x1b$)C\x0e\x30\x21\x0f"),
                                   ("euc_jis_2004", b"\xa1\xa1"), ("shift_jis_2004", b"\x81\x40"),
                                   ("iso2022_jp_2", b"abc"), ("idna", b"abc"),
                                   ("unicode_escape", b"\\N{LATIN SMALL LETTER A}")]):
        d = _write(f"r3-missing-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw)]))
        yield _tg("almanac", d, ["xx"], ["Size"])
    # every alias Python's codecs know, through a .mo charset (ASCII text decodes in nearly all)
    import encodings.aliases
    for i, alias in enumerate(sorted(encodings.aliases.aliases)):
        d = _write(f"r3-alias-{i}", "xx", _mo([(b"", _header(charset=alias)), (b"Size", b"Size")]))
        yield _tg("almanac", d, ["xx"], ["Size"])
    # every key of locale.locale_alias, and each with an encoding and a modifier
    for k in sorted(locale.locale_alias):
        for loc in [k, k + ".utf8", k + "@euro", k.upper()]:
            yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
            yield "expand_lang", {"loc": loc}, (lambda loc=loc: gettext._expand_lang(loc))
    # str.isalnum() over the BMP and beyond, through normalize_encoding
    cps = [c for c in range(0x80, 0x323B0) if not (0xD800 <= c <= 0xDFFF) and chr(c) not in ".@:"]
    for s in range(0, len(cps), 3000):
        loc = "de.a" + "".join(chr(c) + "a" for c in cps[s:s + 3000])
        yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
    # a language that is a real character in the plane the port uses for undecodable env bytes
    pua = "\U0010FD80"
    d = os.path.join(_TMP, "r3-pua")
    os.makedirs(os.path.join(d, pua, "LC_MESSAGES"), exist_ok=True)
    with open(os.path.join(d, pua, "LC_MESSAGES", "almanac.mo"), "wb") as f:
        f.write(_mo([(b"", _header()), (b"Size", b"Gr")]))
    os.makedirs(os.path.join(os.fsencode(d), b"\x80", b"LC_MESSAGES"), exist_ok=True)
    with open(os.path.join(os.fsencode(d), b"\x80", b"LC_MESSAGES", b"almanac.mo"), "wb") as f:
        f.write(_mo([(b"", _header()), (b"Size", b"raw80")]))
    yield "find", {"domain": "almanac", "localedir": d, "languages": [pua]}, \
        (lambda d=d: gettext.find("almanac", localedir=d, languages=[pua], all=True))
    yield _tg("almanac", d, [pua], ["Size"])
    # odd language strings reaching os.path.join
    d = _write("r3-paths", "de", _mo([(b"", _header()), (b"Size", b"G")]))
    os.makedirs(os.path.join(d, "LC_MESSAGES"), exist_ok=True)
    shutil.copy(os.path.join(d, "de", "LC_MESSAGES", "almanac.mo"), os.path.join(d, "LC_MESSAGES", "almanac.mo"))
    weird = [".", "", "..", "de/", "./de", "de/../de", "/nonexistent", "de//", " de", "de\t"]
    yield "find", {"domain": "almanac", "localedir": d, "languages": weird}, \
        (lambda d=d: gettext.find("almanac", localedir=d, languages=weird, all=True))
    yield "find", {"domain": "almanac", "localedir": d + "/", "languages": ["de"]}, \
        (lambda d=d: gettext.find("almanac", localedir=d + "/", languages=["de"], all=True))
    yield "find", {"domain": "almanac", "localedir": "", "languages": ["de"]}, \
        (lambda: gettext.find("almanac", localedir="", languages=["de"], all=True))
    yield _tg("almanac", d, weird, ["Size"])
    # final sigma, dotted I and friends through str.lower()
    for loc in ["ΣΑΣ.ΣΑΣ", "İ", "DE_DE.UTF-8@EURO", "Ⅻ", "ǅ_ǅ", "ẞ", "ﬃ.utf8"]:
        yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
    # str.lower() (Unicode 16 in CPython 3.14) vs Rust's to_lowercase (newer Unicode):
    # U+A7D2/U+A7D4 have no lowercase in Python, so normalize_encoding sees a non-alnum
    for loc in ["de.a꟒a", "de.a꟔a"]:
        yield "normalize", {"loc": loc}, (lambda loc=loc: locale.normalize(loc))
        yield "expand_lang", {"loc": loc}, (lambda loc=loc: gettext._expand_lang(loc))

    # --- review round 3 fixes ---
    yield from _round3()


def _codec(cs, raw):
    return "codec_decode", {"charset": cs, "hex": raw.hex()}, (lambda: ascii(str(raw, cs)))


def _round3():
    import random
    import time
    import unicodedata
    from pathlib import Path
    rnd = random.Random(20261017)
    # every multibyte codec: valid sequences, cut ones and garbage
    for cs in ["gb2312", "gbk", "gb18030", "big5", "cp950", "big5hkscs", "cp932", "shift_jis", "euc_jp",
               "euc_jis_2004", "euc_jisx0213", "shift_jis_2004", "shift_jisx0213", "euc_kr", "cp949", "johab"]:
        pool = []
        for _ in range(4000):
            n = rnd.choice([1, 2, 2, 2, 3, 4])
            b = bytes([rnd.choice([rnd.randrange(0x80, 0x100), rnd.randrange(0x20, 0x80), rnd.randrange(0x30, 0x3a)])
                       for _ in range(n)])
            try:
                b.decode(cs)
                pool.append(b)
            except UnicodeDecodeError:
                pass
        for _ in range(250):
            parts = []
            for _ in range(rnd.randrange(1, 6)):
                r = rnd.random()
                if r < 0.7 and pool:
                    parts.append(rnd.choice(pool))
                elif r < 0.8 and cs == "euc_kr":
                    parts.append(b"\xa4\xd4\xa4" + bytes([rnd.randrange(0xa0, 0xc0)]) + b"\xa4"
                                 + bytes([rnd.randrange(0xbd, 0xd5)]) + b"\xa4" + bytes([rnd.randrange(0xa0, 0xd6)]))
                else:
                    parts.append(bytes([rnd.randrange(256) for _ in range(rnd.randrange(1, 3))]))
            yield _codec(cs, b"".join(parts))
    # the ISO-2022 family and HZ: designations, shifts, SS2, stray escapes
    escapes = [b"\x1b(B", b"\x1b(J", b"\x1b(I", b"\x1b$@", b"\x1b$B", b"\x1b$A", b"\x1b$(C", b"\x1b$(D",
               b"\x1b$(O", b"\x1b$(P", b"\x1b$(Q", b"\x1b$)C", b"\x1b$)A", b"\x1b.A", b"\x1b.F", b"\x1b.B",
               b"\x1b.J", b"\x1bN", b"\x1b&@\x1b$B", b"\x1b&@", b"\x1b(", b"\x1bx", b"\x1b$", b"\x1b(Z",
               b"\x1b$)D", b"\x1b$(B", b"\x1b(Ia", b"\x0e", b"\x0f", b"\n", b"\x1b"]
    for cs in ["iso2022_jp", "iso2022_jp_1", "iso2022_jp_2", "iso2022_jp_2004", "iso2022_jp_3", "iso2022_jp_ext",
               "iso2022_kr", "csiso2022jp", "iso-2022-jp-2"]:
        for _ in range(250):
            parts = []
            for _ in range(rnd.randrange(1, 8)):
                r = rnd.random()
                if r < 0.35:
                    parts.append(rnd.choice(escapes))
                elif r < 0.75:
                    parts.append(bytes([rnd.randrange(0x21, 0x7f), rnd.randrange(0x21, 0x7f)]))
                elif r < 0.9:
                    parts.append(bytes([rnd.randrange(0x00, 0x80)]))
                else:
                    parts.append(bytes([rnd.randrange(256)]))
            yield _codec(cs, b"".join(parts))
    for _ in range(300):
        parts = []
        for _ in range(rnd.randrange(1, 8)):
            parts.append(rnd.choice([b"~{", b"~}", b"~~", b"~\n", b"~x", b"~", b"a", b"\n",
                                     bytes([rnd.randrange(0x21, 0x7f), rnd.randrange(0x21, 0x7f)]),
                                     bytes([rnd.randrange(256)])]))
        yield _codec(rnd.choice(["hz", "hz-gb-2312"]), b"".join(parts))
    # UTF-7: encoded text (astral characters and lone surrogates) and garbage
    alphabet = "aZ09+-/ \t\n~\\é€\U0001F389𐀀\udfff"
    for _ in range(300):
        s = "".join(rnd.choice(alphabet) for _ in range(rnd.randrange(0, 8)))
        raw = s.encode("utf-7", "surrogatepass") if rnd.random() < 0.6 else \
            bytes(rnd.choice(b"+-AZaz09/!\n\x80 ") for _ in range(rnd.randrange(1, 10)))
        yield _codec("utf-7", raw)
    # Punycode and IDNA: real labels, upper case, mangled ones, nameprep's cases
    words = ["münchen", "größe", "日本語", "bücher", "ÄÖÜ", "ǅ", "ﬃ", "Ａｂｃ", "a­b", "عربي", "عربيa",
             "שלום", "́x", "é", "각", "\U0001F389", "a‍b", "xn--a", "ss", "ß",
             "İ", "K", "x" * 70 + "é", "café", "naïve", "．", "。", "שּׁ", "ال"]
    for w in words:
        for label in [w, w.upper(), w.lower()]:
            try:
                raw = b"xn--" + label.encode("punycode")
            except Exception:
                continue
            for x in [raw, raw.upper(), raw + b".de", b"www." + raw + b".", raw[:-1], raw + b"a"]:
                yield _codec("idna", x)
            yield _codec("punycode", label.encode("punycode"))
    for _ in range(300):
        raw = bytes(rnd.choice(b"abz09-AZ.xn-") for _ in range(rnd.randrange(1, 14)))
        yield _codec(rnd.choice(["punycode", "idna"]), raw)
        yield _codec("idna", b"xn--" + raw)
    yield _codec("idna", b"")
    yield _codec("idna", "ü.de".encode())
    yield _codec("idna", b"a" * 1100)
    yield _codec("idna", b"xn--" + b"a" * 1100)
    # unicode_escape and raw_unicode_escape: surrogates, octal past 0o377,
    # \N{} by every name, alias and rule
    for raw in [rb"\ud800", rb"\udfff\ud800", rb"\777", rb"\400", rb"\U0010ffff", rb"\U00110000", rb"\x",
                rb"\N", rb"\N{", rb"\N{}", rb"\N{LATIN SMALL LETTER A", rb"\Nx", rb"\N{latin small letter a}",
                rb"\N{LATIN CAPITAL LETTER A WITH MACRON AND GRAVE}", rb"\N{BYTE ORDER MARK}", rb"\N{BOM}",
                rb"\N{HANGUL SYLLABLE GA}", rb"\N{HANGUL SYLLABLE GAG}", rb"\N{HANGUL SYLLABLE KIYEOK}",
                rb"\N{HANGUL SYLLABLE }", rb"\N{HANGUL SYLLABLE GAGX}", rb"\N{HANGUL SYLLABLE A}",
                rb"\N{HANGUL SYLLABLE YAE}", rb"\N{hangul syllable ssyaenh}", rb"\N{HANGUL SYLLABLE}",
                rb"\N{CJK UNIFIED IDEOGRAPH-4E00}", rb"\N{CJK UNIFIED IDEOGRAPH-04E00}", rb"\N{CJK UNIFIED IDEOGRAPH-4e0}",
                rb"\N{CJK UNIFIED IDEOGRAPH-20000}", rb"\N{CJK UNIFIED IDEOGRAPH-2A6E0}", rb"\N{CJK UNIFIED IDEOGRAPH-4E00 }",
                rb"\N{CJK UNIFIED IDEOGRAPH-}", rb"\N{TANGUT IDEOGRAPH-17000}", rb"\N{TANGUT IDEOGRAPH-187F8}",
                rb"\N{TANGUT IDEOGRAPH-18D08}", rb"\N{CJK COMPATIBILITY IDEOGRAPH-F900}", rb"\N{KHITAN SMALL SCRIPT CHARACTER-18B00}",
                rb"\N{NUSHU CHARACTER-1B170}", rb"\N{EGYPTIAN HIEROGLYPH-13460}", rb"\N{TANGUT COMPONENT-001}",
                rb"\N{CJK UNIFIED IDEOGRAPH-110000}", rb"\N{CJK UNIFIED IDEOGRAPH-1000000}", rb"\N{\xff}",
                b"\\N{LATIN SMALL LETTER \xe9}", rb"\N{NULL}", rb"\N{LINE FEED}", rb"\N{lf}", rb"\N{ZWJ}",
                rb"\N{LATIN CAPITAL LETTER GHA}", rb"\N{  LATIN SMALL LETTER A}"]:
        yield _codec("unicode_escape", raw)
    for raw in [rb"\ud800", rb"\U00110000", rb"\u12", rb"\\u0041", rb"\\A", b"\\", b"a\\", rb"\x41", b"\\\xe9"]:
        yield _codec("raw_unicode_escape", raw)
    names = []
    prev = ""
    for line in (Path(__file__).resolve().parent.parent / "data" / "i18n" / "names.txt").read_text().splitlines():
        rest, _hex = line[1:].rsplit(";", 1)
        name = prev[:ord(line[0]) - ord("0")] + rest
        names.append(name)
        prev = name
    for k in range(0, len(names), 250):
        chunk = names[k:k + 250]
        raw = "".join("\\N{%s}" % (n.lower() if i % 3 == 0 else n) for i, n in enumerate(chunk)).encode()
        yield _codec("unicode_escape", raw)
    hangul = []
    for cp in range(0xAC00, 0xD7A4, 7):
        hangul.append(unicodedata.name(chr(cp)))
    for k in range(0, len(hangul), 200):
        raw = "".join("\\N{%s}" % n for n in hangul[k:k + 200]).encode()
        yield _codec("unicode_escape", raw)
    # surrogates in a catalog: ascii() shows what JSON cannot carry
    for i, (cs, raw) in enumerate([("utf-7", b"+2AA-"), ("utf-7", b"+2D3cAQ-"), ("unicode_escape", rb"x\udc80y"),
                                   ("punycode", b"9g8a"), ("raw_unicode_escape", rb"\udfff")]):
        d = _write(f"r4-sur-{i}", "xx", _mo([(b"", _header(charset=cs)), (b"Size", raw), (b"Other", b"plain")]))
        yield "translation_gettext_ascii", {"domain": "almanac", "localedir": d, "languages": ["xx"],
                                            "messages": ["Size", "Other", "\U0010FD80"]}, \
            (lambda d=d: [ascii(_py_t("almanac", d, ["xx"]).gettext(m)) for m in ["Size", "Other", "\U0010FD80"]])
    # a msgid with a real character from the plane the port keeps surrogates in
    d = _write("r4-plane16", "xx", _mo([(b"", _header()), ("\U0010FD80\U0010F7FF".encode(), "ok \U0010FFFF".encode())]))
    yield _tg("almanac", d, ["xx"], ["\U0010FD80\U0010F7FF", "\U0010FD80"])
    # str.lower() over every code point (surrogates aside), and final sigma
    cps = [c for c in range(0x110000) if not 0xD800 <= c <= 0xDFFF]
    for s in range(0, len(cps), 4000):
        text = "".join(chr(c) for c in cps[s:s + 4000])
        yield "str_lower", {"s": text}, (lambda text=text: text.lower())
    sig = ["Σ", "A", "a", ".", "'", "́", " ", "1", "­", "ǅ", "ʰ", "Ω"]
    for _ in range(300):
        text = "".join(rnd.choice(sig) for _ in range(rnd.randrange(1, 7)))
        yield "str_lower", {"s": text}, (lambda text=text: text.lower())
        yield "normalize", {"loc": text}, (lambda text=text: locale.normalize(text))
    # ngettext's count: floats (with gettext's DeprecationWarning), and what is not a number
    nf = {"nan": float("nan"), "inf": float("inf"), "-inf": float("-inf")}
    for n in [1.0, 1.5, 0.0, -1.0, 2.5, 21.0, 1e300, 0.5]:
        yield "ngettext", {"singular": "one app", "plural": "%d apps", "n": n}, \
            (lambda n=n: i18n.ngettext("one app", "%d apps", n))
    for k, v in nf.items():
        yield "ngettext", {"singular": "one", "plural": "many", "n": {"float": k}}, \
            (lambda v=v: i18n.ngettext("one", "many", v))
    for v, j in [("x", "x"), (None, None), (True, True), (False, False)]:
        yield "ngettext", {"singular": "one", "plural": "many", "n": j}, (lambda v=v: i18n.ngettext("one", "many", v))
    for p in ["n != 1", "n", "n%10==1 && n%100!=11 ? 0 : 1", "n/2", "n%3", "n*n > 2", "n || 3", "n && 3",
              "n == 1.5", "n/0", "n%0", "1 < n < 3", "n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n*n"]:
        ns = [1.0, 1.5, 0.0, -1.5, 2.0, 21.0, 111.0, 2.5e10, 1e300, -0.0, 0.1, 7.75]
        yield "c2py", {"plural": p, "ns": ns}, (lambda p=p, ns=ns: [gettext.c2py(p)(n) for n in ns])
        for n in ns:
            yield "c2py", {"plural": p, "ns": [n]}, (lambda p=p, n=n: [gettext.c2py(p)(n)])
    for p in ["n != 1", "n/0", "n"]:
        for k, v in nf.items():
            yield "c2py", {"plural": p, "ns": [{"float": k}]}, (lambda p=p, v=v: [gettext.c2py(p)(v)])
        yield "c2py", {"plural": p, "ns": ["x"]}, (lambda p=p: [gettext.c2py(p)("x")])
    d = _write("r4-float", "de", _mo([(b"", _header(plural="nplurals=3; plural=n==1 ? 0 : n<5 ? 1 : 2;")),
                                      (b"one\0many", b"eins\0wenige\0viele")]))
    for n in [1.0, 1.5, 4.9, 5.0, 0.0, -3.0, True]:
        yield _tn("almanac", d, ["de"], ["one", "missing"], "many", n)
    for k, v in nf.items():
        yield "translation_ngettext", {"domain": "almanac", "localedir": d, "languages": ["de"],
                                       "messages": ["one"], "plural": "many", "n": {"float": k}}, \
            (lambda v=v, d=d: [_py_t("almanac", d, ["de"]).ngettext("one", "many", v)])
    yield "translation_ngettext", {"domain": "almanac", "localedir": d, "languages": ["de"],
                                   "messages": ["one"], "plural": "many", "n": "x"}, \
        (lambda d=d: [_py_t("almanac", d, ["de"]).ngettext("one", "many", "x")])
    d = _write("r4-float-default", "de", _mo([(b"", _header()), (b"one\0many", b"eins\0viele")]))
    for n in [1.0, 1.5, "x", None]:
        yield "translation_ngettext", {"domain": "almanac", "localedir": d, "languages": ["de"],
                                       "messages": ["one", "missing"], "plural": "many", "n": n}, \
            (lambda n=n, d=d: [_py_t("almanac", d, ["de"]).ngettext(m, "many", n) for m in ["one", "missing"]])
    yield "translation_ngettext", {"domain": "almanac", "localedir": d, "languages": ["de"],
                                   "messages": ["one"], "plural": "many", "n": {"float": "nan"}}, \
        (lambda d=d: [_py_t("almanac", d, ["de"]).ngettext("one", "many", float("nan"))])
    # gettext's default directory and LC_TIME after the import-time setlocale
    yield "default_localedir", {}, lambda: gettext._default_localedir
    def lc_time():
        tm = lambda mon, mday, wday: (2023, mon, mday, 12, 0, 0, wday, 1, 0)
        return {"abday": [time.strftime("%a", tm(1, 1 + i, (6 + i) % 7)) for i in range(7)],
                "day": [time.strftime("%A", tm(1, 1 + i, (6 + i) % 7)) for i in range(7)],
                "abmon": [time.strftime("%b", tm(m, 1, 0)) for m in range(1, 13)],
                "mon": [time.strftime("%B", tm(m, 1, 0)) for m in range(1, 13)]}
    yield "lc_time", {}, lc_time
