# Natural writing and Unicode review

## Investigation and adaptation

Sources reviewed on 2026-11-24 (upstream branches are mutable):

- [blader/humanizer](https://github.com/blader/humanizer): an instruction-based
  prose editor, not a detector. Its README distinguishes writing for readers
  from bypassing AI detectors. Useful ideas: remove stock introductions,
  inflated significance, vague attribution and repetitive transitions; match
  the author's voice and preserve claims. Do not turn stylistic examples into
  forbidden-word lists: a dash, a triad or a technical term can be appropriate.
- [guillaumemeyer/watermarks-remover](https://github.com/guillaumemeyer/watermarks-remover):
  separates deterministic Unicode handling, model-based rewriting and file
  provenance/metadata handling. Reviewed its README and
  [clean_text.py](https://github.com/guillaumemeyer/watermarks-remover/blob/main/service/scripts/clean_text.py)
  and [inspect_text.py](https://github.com/guillaumemeyer/watermarks-remover/blob/main/service/scripts/inspect_text.py).
  The CLI exposes optional NFKC, confusable, bidi and emoji-glue transformations;
  these are not safe generic operations on arbitrary source files. Its README
  documents same-key/configuration limits for statistical detectors. Neither
  a Unicode scan nor a stylistic score establishes authorship or absence of a
  statistical watermark. Vendor/API claims in that README were not independently
  validated here.

Nexo's normal and compact built-in prompts already include concise natural
writing and preservation guidance (`crates/nexo-agent/src/prompt.md` and
`prompt_compact.md`). Keep that lightweight integration rather than loading
an entire external skill into every conversation. Edit prose for the request:
state the result directly, retain uncertainty and citations, and preserve the
writer's register. Never invent facts, opinions, experiences or citations to
make text seem human. Code, paths, link targets, quoted text and structured data
are not incidental prose to rewrite.

No upstream installer, service, dependency, rewrite hook or metadata remover is
installed by this adaptation. There is no automatic output filter or promise
of detection/evasion. Raw/custom prompt users retain their own instructions.

## Optional, non-destructive Unicode inspection

From the `nexo/` directory:

```sh
python3 tools/inspect-unicode.py docs/example.md
nice -n 10 python3 -B tools/test_inspect_unicode.py
```

The inspector uses only Python's standard library, reads a named UTF-8 file and
prints JSON locations/names for format controls, non-ASCII spaces, variation
selectors and unexpected controls. It does **not** print the surrounding text,
write files, normalize Unicode, call a model, or return an AI score. Exit 0 means
the inspection ran (even with findings); 2 means the file could not be inspected.
Offsets are Unicode code points, not UTF-8 bytes or terminal display columns.
Known binary signatures, NUL-bearing input and invalid UTF-8 are refused; this
is not a universal file-format detector. Do not pass binary or container files.
It is an opt-in developer utility, not a new built-in tool or a default gate.

Treat each finding as a review location, **not** an error or watermark:

- ZWJ/variation selectors/tags can construct emoji; ZWNJ can carry orthographic
  meaning in Persian. Bidi controls can be necessary for mixed RTL/LTR text.
- Nonbreaking spaces can preserve units and typography. A BOM can identify an
  encoding. Combining marks and non-Latin letters must not be ASCII-folded.
- Source code, test fixtures, literal strings, quotations and signed data can
  deliberately contain any of these characters. NFKC can change identifiers,
  mathematical notation and compatibility characters.

If the person wants cleanup, inspect first, confirm which occurrences are
unwanted, then make a minimal explicit edit to those occurrences. Review the
actual diff and run the relevant format/parser/tests; leave meaningful Unicode
and provenance intact. Do not blanket-strip all `Cf` characters or normalize
all spaces. Do not silently rewrite chat output, tool results or files.

Tests cover meaningful joiners, bidi, emoji tags/selectors, NBSP, multilingual
text, combining marks, CRLF/code-point locations, binary/invalid input and
byte/mtime preservation. They verify the utility's behavior, not subjective
writing quality or statistical watermark detection.
