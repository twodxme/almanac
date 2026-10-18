#!/usr/bin/env python3
"""Generate the tables src/i18n.rs carries from Python's own codecs and
Unicode database (CPython 3.14), so the port decodes a catalog exactly as
`str(bytes, charset)` does.

    python3 rust/parity/i18n_tables.py NameAliases.txt CompositionExclusions.txt

Both files are Unicode 16.0's (https://www.unicode.org/Public/16.0.0/ucd/),
the version `unicodedata.unidata_version` reports; every alias is checked
against Python's own lookup.

Writes rust/data/i18n/codecs.txt and rust/data/i18n/names.txt, and prints
the Rust tables for `str.lower()` to paste into src/i18n.wip.rs.
"""
import json
import sys
import unicodedata
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "data" / "i18n"

# The stateless multibyte codecs: every byte sequence is explored through
# Python's decoder, one byte at a time.
STATELESS = ["gb2312", "gbk", "gb18030", "big5", "cp950", "big5hkscs", "cp932", "shift_jis",
             "euc_jp", "euc_jis_2004", "euc_jisx0213", "shift_jis_2004", "shift_jisx0213",
             "euc_kr", "cp949", "johab"]

ILLEGAL, LEAD = None, 1


def classify(codec, seq):
    """What Python makes of `seq` as one unit: a string, ILLEGAL, or LEAD."""
    try:
        return seq.decode(codec)
    except UnicodeDecodeError as e:
        if e.reason == "incomplete multibyte sequence":
            return LEAD
        return ILLEGAL


def explore(codec, prefix=b"", special=lambda codec, seq: None):
    """{prefix: [256 entries]} for every prefix that is a lead."""
    tables = {}
    entries = []
    for b in range(256):
        seq = prefix + bytes([b])
        sp = special(codec, seq)
        e = ILLEGAL if sp == "skip" else classify(codec, seq)
        entries.append(e)
    tables[prefix] = entries
    for b, e in enumerate(entries):
        if e is LEAD:
            sub = explore(codec, prefix + bytes([b]), special)
            # a lead whose every continuation is illegal is just illegal
            if all(x is ILLEGAL for x in sub[prefix + bytes([b])]) and len(sub) == 1:
                entries[b] = ILLEGAL
            else:
                tables.update(sub)
    return tables


def special(codec, seq):
    # gb18030's four-byte sequences and euc_kr's eight-byte make-up
    # sequences are decoded by rule in the port, not by table.
    if codec == "gb18030" and len(seq) == 2 and 0x30 <= seq[1] <= 0x39:
        return "skip"
    if codec == "euc_kr" and seq == b"\xa4\xd4":
        return "skip"
    return None


def encode_tables(tables):
    """{prefix hex: [[start, chars], ...]} runs of one-character entries,
    and {sequence hex: string} for entries of another length."""
    t, m = {}, {}
    for prefix, entries in tables.items():
        runs = []
        start, chars = None, ""
        for b, e in enumerate(entries + [ILLEGAL]):
            if isinstance(e, str) and len(e) == 1:
                if start is None:
                    start, chars = b, ""
                chars += e
                continue
            if start is not None:
                runs.append([start, chars])
                start = None
            if isinstance(e, str):
                m[(prefix + bytes([b])).hex()] = e
        t[prefix.hex()] = runs
    return {"t": t, "m": m}


def iso2022_tables():
    """The character sets of the ISO-2022 codecs, each read through Python
    with only that set designated: {name: tables}. A double-byte set is a
    lead (0x20..0x7f) and any second byte, as the decoder consumes them."""
    sets = {
        "jisx0208": ("iso2022_jp", b"\x1b$B", 2),
        "jisx0212": ("iso2022_jp_1", b"\x1b$(D", 2),
        "ksx1001": ("iso2022_kr", b"\x1b$)C\x0e", 2),
        "gb2312": ("iso2022_jp_2", b"\x1b$A", 2),
        "jisx0213_2000_1": ("iso2022_jp_3", b"\x1b$(O", 2),
        "jisx0213_2000_2": ("iso2022_jp_3", b"\x1b$(P", 2),
        "jisx0213_2004_1": ("iso2022_jp_2004", b"\x1b$(Q", 2),
        "jisx0213_2004_2": ("iso2022_jp_2004", b"\x1b$(P", 2),
        "jisx0201_r": ("iso2022_jp", b"\x1b(J", 1),
        "jisx0201_k": ("iso2022_jp_ext", b"\x1b(I", 1),
    }
    out = {}
    for name, (codec, esc, width) in sets.items():
        def dec(seq):
            try:
                return (esc + seq).decode(codec)
            except UnicodeDecodeError:
                return ILLEGAL
        root = [ILLEGAL] * 256
        tables = {b"": root}
        for c1 in range(0x20, 0x80):
            if width == 1:
                root[c1] = dec(bytes([c1]))
                continue
            row = [dec(bytes([c1, c2])) for c2 in range(256)]
            if any(x is not ILLEGAL for x in row):
                root[c1] = LEAD
                tables[bytes([c1])] = row
        out[name] = tables
    # HZ's GB2312: any 7-bit lead but '~', any second byte
    root = [ILLEGAL] * 256
    tables = {b"": root}
    for c1 in range(0x80):
        if c1 == ord("~"):
            continue
        row = []
        for c2 in range(256):
            try:
                row.append((b"~{" + bytes([c1, c2])).decode("hz"))
            except UnicodeDecodeError:
                row.append(ILLEGAL)
        if any(x is not ILLEGAL for x in row):
            root[c1] = LEAD
            tables[bytes([c1])] = row
    out["hz_gb2312"] = tables
    return out


def gb18030_ranges():
    """The four-byte BMP sequences: [lseq, code point] where a run of
    consecutive code points starts."""
    runs = []
    prev = None
    for lseq in range(39420):
        c, r = divmod(lseq, 1260 * 10)
        c2, r = divmod(r, 1260)
        c3, c4 = divmod(r, 10)
        seq = bytes([0x81 + c, 0x30 + c2, 0x81 + c3, 0x30 + c4])
        cp = ord(seq.decode("gb18030"))
        if prev is None or cp != prev + 1:
            runs.append([lseq, cp])
        prev = cp
    return runs


def euckr_jamo():
    """cgk2u_choseong / cgk2u_jongseong for bytes 0xa1..0xbe (-1: none)."""
    def cp(seq):
        try:
            return ord(seq.decode("euc_kr")) - 0xAC00
        except UnicodeDecodeError:
            return None
    cho = []
    for c in range(0xA1, 0xBF):
        v = cp(b"\xa4\xd4\xa4" + bytes([c]) + b"\xa4\xbf\xa4\xd4")
        cho.append(-1 if v is None else v // 588)
    first = cho.index(0) + 0xA1
    jong = []
    for c in range(0xA1, 0xBF):
        v = cp(b"\xa4\xd4\xa4" + bytes([first]) + b"\xa4\xbf\xa4" + bytes([c]))
        jong.append(-1 if v is None else v % 28)
    return {"cho": cho, "jong": jong}


def idna_tables(exclusions_path):
    """What the `idna` codec's nameprep needs: stringprep's tables over every
    code point (surrogates included), Unicode 3.2 compatibility
    decompositions as `unicodedata.ucd_3_2_0.normalize('NFKD')` gives them
    one character at a time, and the composition pairs and combining classes
    its NFKC reads from the current database (CPython composes and orders
    with Unicode 16.0's tables, whatever the version object)."""
    import stringprep
    from unicodedata import ucd_3_2_0
    allcp = range(0x110000)
    b1 = ranges(lambda cp: stringprep.in_table_b1(chr(cp)), surrogates=True)
    b2 = [[cp, stringprep.map_table_b2(chr(cp))] for cp in allcp
          if stringprep.map_table_b2(chr(cp)) != chr(cp)]
    cs = [stringprep.in_table_c12, stringprep.in_table_c22, stringprep.in_table_c3, stringprep.in_table_c4,
          stringprep.in_table_c5, stringprep.in_table_c6, stringprep.in_table_c7, stringprep.in_table_c8,
          stringprep.in_table_c9]
    prohibited = ranges(lambda cp: any(f(chr(cp)) for f in cs), surrogates=True)
    d1 = ranges(lambda cp: stringprep.in_table_d1(chr(cp)), surrogates=True)
    d2 = ranges(lambda cp: stringprep.in_table_d2(chr(cp)), surrogates=True)
    nfkd = [[cp, ucd_3_2_0.normalize("NFKD", chr(cp))] for cp in allcp
            if not 0xAC00 <= cp <= 0xD7A3 and ucd_3_2_0.normalize("NFKD", chr(cp)) != chr(cp)]
    ccc = []
    for cp in allcp:
        c = unicodedata.combining(chr(cp))
        if c and ccc and ccc[-1][1] == cp - 1 and ccc[-1][2] == c:
            ccc[-1][1] = cp
        elif c:
            ccc.append([cp, cp, c])
    excluded = set()
    for line in Path(exclusions_path).read_text().splitlines():
        line = line.split("#")[0].strip()
        if line:
            excluded.add(int(line, 16))
    comp = []
    for cp in allcp:
        d = unicodedata.decomposition(chr(cp))
        if not d or d.startswith("<"):
            continue
        parts = [int(x, 16) for x in d.split()]
        if len(parts) == 2 and cp not in excluded and unicodedata.combining(chr(parts[0])) == 0:
            comp.append([parts[0], parts[1], cp])
    return {"b1": b1, "b2": b2, "c": prohibited, "d1": d1, "d2": d2, "nfkd": nfkd, "ccc": ccc, "comp": comp}


def codecs_file(exclusions_path):
    lines = ["idna " + json.dumps(idna_tables(exclusions_path), ensure_ascii=False, separators=(",", ":"))]
    for codec in STATELESS:
        lines.append(codec + " " + json.dumps(encode_tables(explore(codec, special=special)),
                                              ensure_ascii=False, separators=(",", ":")))
    for name, tables in iso2022_tables().items():
        lines.append("iso2022_" + name + " " + json.dumps(encode_tables(tables), ensure_ascii=False,
                                                          separators=(",", ":")))
    lines.append("gb18030_ranges " + json.dumps(gb18030_ranges(), separators=(",", ":")))
    lines.append("euc_kr_jamo " + json.dumps(euckr_jamo(), separators=(",", ":")))
    return "\n".join(lines) + "\n"


DERIVED = [(0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xAC00, 0xD7A3), (0x17000, 0x187F7), (0x18D00, 0x18D08),
           (0x20000, 0x2A6DF), (0x2A700, 0x2B739), (0x2B740, 0x2B81D), (0x2B820, 0x2CEA1),
           (0x2CEB0, 0x2EBE0), (0x2EBF0, 0x2EE5D), (0x30000, 0x3134A), (0x31350, 0x323AF)]


def names_file(aliases_path):
    """Sorted `NAME;hex` lines, front-coded: each line starts with how many
    leading characters it shares with the line before, as one character
    ('0' + n)."""
    names = {}
    for cp in range(0x110000):
        if any(lo <= cp <= hi for lo, hi in DERIVED):
            continue
        n = unicodedata.name(chr(cp), None)
        if n is not None:
            names[n] = cp
    for line in Path(aliases_path).read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        cp, alias, _kind = line.split(";")
        cp = int(cp, 16)
        assert unicodedata.lookup(alias) == chr(cp), alias
        assert ("\\N{%s}" % alias).encode().decode("unicode_escape") == chr(cp), alias
        names.setdefault(alias, cp)
    out, prev = [], ""
    for n in sorted(names):
        k = 0
        while k < min(len(n), len(prev), 74) and n[k] == prev[k]:
            k += 1
        out.append(chr(ord("0") + k) + n[k:] + ";" + format(names[n], "x"))
        prev = n
    return "\n".join(out) + "\n"


def ranges(pred, surrogates=False):
    out, start = [], None
    for cp in range(0x110001):
        ok = cp < 0x110000 and (surrogates or not (0xD800 <= cp <= 0xDFFF)) and pred(cp)
        if ok and start is None:
            start = cp
        elif not ok and start is not None:
            out.append((start, cp - 1))
            start = None
    return out


def lower_tables():
    """`str.lower()` per character, and the Cased / Case_Ignorable sets its
    final-sigma rule reads, inferred from what lower() does around 'Σ'."""
    lower = []
    for cp in range(0x110000):
        if 0xD800 <= cp <= 0xDFFF or cp == 0x3A3:
            continue
        lo = chr(cp).lower()
        if lo != chr(cp):
            lower.append((cp, lo))
    # 'AΣ' + c: σ when c is cased and not case-ignorable;
    # 'AΣ' + c + 'A': ς when c is neither cased nor case-ignorable
    cased = ranges(lambda cp: ("AΣ" + chr(cp)).lower()[1] == "σ")
    ignorable = ranges(lambda cp: ("AΣ" + chr(cp)).lower()[1] == "ς"
                       and ("AΣ" + chr(cp) + "A").lower()[1] == "σ")
    return lower, cased, ignorable


def rust_lower(lower, cased, ignorable):
    def esc(s):
        return "".join(c if c.isascii() and c.isprintable() and c not in "\\\"" else "\\u{%x}" % ord(c) for c in s)
    out = ["/// `str.lower()` for every character it changes (CPython 3.14, Unicode",
           "/// 16.0), generated from Python; 'Σ' is left to the final-sigma rule.",
           "const PY_LOWER: &[(u32, &str)] = &["]
    for cp, lo in lower:
        out.append('    (0x%x, "%s"),' % (cp, esc(lo)))
    out.append("];")
    for name, rs, doc in [("PY_CASED", cased, "Cased and not Case_Ignorable"),
                          ("PY_CASE_IGNORABLE", ignorable, "Case_Ignorable")]:
        out.append(f"/// Characters `str.lower()` treats as {doc} around a final 'Σ'.")
        out.append(f"const {name}: &[(u32, u32)] = &[")
        for lo, hi in rs:
            out.append("    (0x%x, 0x%x)," % (lo, hi))
        out.append("];")
    return "\n".join(out) + "\n"


def main(argv):
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "codecs.txt").write_text(codecs_file(argv[1]), encoding="utf-8")
    (OUT / "names.txt").write_text(names_file(argv[0]), encoding="utf-8")
    sys.stdout.write(rust_lower(*lower_tables()))


if __name__ == "__main__":
    main(sys.argv[1:])
