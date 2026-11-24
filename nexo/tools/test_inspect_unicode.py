"""Offline CLI tests: findings are advisory and source bytes never change."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("inspect-unicode.py")


class InspectUnicodeTests(unittest.TestCase):
    def run_inspector(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.txt"
            path.write_bytes(data)
            before = path.stat().st_mtime_ns
            result = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True)
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(path.stat().st_mtime_ns, before)
            self.assertEqual(list(Path(directory).iterdir()), [path])
            return result

    def test_plain_multilingual_and_code_remain_unchanged(self):
        text = 'Español, 中文, العربية, e\u0301.\r\n```rust\nlet x = "—";\n```\n'
        result = self.run_inspector(text.encode())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["findings"], [])

    def test_meaningful_joiners_bidi_spaces_emoji_and_tags_are_review_only(self):
        text = '\ufeffمی\u200cروم 👩\u200d💻 ❤️ \u2067العربية\u2069 10\u00a0kg 🏴\U000e0067\U000e0062\U000e007f'
        result = self.run_inspector(text.encode())
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        points = {item["codepoint"] for item in report["findings"]}
        self.assertTrue({"U+FEFF", "U+200C", "U+200D", "U+FE0F", "U+2067", "U+2069", "U+00A0", "U+E0067"} <= points)
        self.assertIn("does not establish AI authorship", report["notice"])

    def test_locations_are_codepoints_not_bytes(self):
        result = self.run_inspector('é\r\n🙂a\u200b'.encode())
        self.assertEqual(result.returncode, 0, result.stderr)
        finding, = json.loads(result.stdout)["findings"]
        self.assertEqual((finding["offset"], finding["line"], finding["column"]), (5, 2, 3))

    def test_binary_and_invalid_utf8_are_refused_without_writes(self):
        for data in [b'PK\x03\x04archive', b'%PDF-1.7', b'\x89PNG\r\n', b'GIF89a', b'\xff\xd8\xff', b'text\0data', b'\xff']:
            with self.subTest(data=data):
                result = self.run_inspector(data)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
                self.assertIn("Cannot inspect", result.stderr)

    def test_missing_file_is_a_clear_error(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, str(SCRIPT), str(Path(directory) / 'missing')], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("Cannot inspect", result.stderr)


if __name__ == "__main__":
    unittest.main()
