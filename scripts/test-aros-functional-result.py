#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 John Knipper
import importlib.util
import json
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('functional_result', ROOT / 'scripts/aros-functional-result.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
EXPECTED = json.loads((ROOT / 'crates/ferail-fs-native/examples/aros-functional-cases.json').read_text())
LINES = [f'FERAIL_FUNCTIONAL case={name} result=PASS' for name in EXPECTED]
FINAL = f'FERAIL_FUNCTIONAL result=PASS passed={len(EXPECTED)} failed=0'


class Coverage(unittest.TestCase):
    def test_complete_manifest_and_explicit_capabilities(self):
        result = m.judge('\n'.join(LINES + ['FERAIL_CAPABILITY example=unavailable', FINAL]), EXPECTED)
        self.assertEqual(result['result'], 'PASS')
        self.assertEqual(result['capabilities'], ['FERAIL_CAPABILITY example=unavailable'])

    def test_missing_duplicate_and_unexpected_cases_rejected(self):
        for lines in (LINES[1:], LINES + [LINES[0]], LINES + ['FERAIL_FUNCTIONAL case=unknown result=PASS']):
            with self.subTest(lines=len(lines)):
                self.assertEqual(m.judge('\n'.join(lines + [FINAL]), EXPECTED)['result'], 'FAIL')

    def test_failures_refusals_and_inconsistent_summaries_rejected(self):
        for lines in (LINES[:-1] + [LINES[-1].replace('PASS', 'FAIL'), FINAL],
                      LINES, LINES + [FINAL, FINAL],
                      LINES + ['FERAIL_FUNCTIONAL result=PASS passed=0 failed=0'],
                      LINES + ['FERAIL_FUNCTIONAL result=REFUSED reason=existing-root', FINAL]):
            with self.subTest(lines=len(lines)):
                self.assertEqual(m.judge('\n'.join(lines), EXPECTED)['result'], 'FAIL')

    def test_invalid_manifests_refused(self):
        for manifest in ([], ['case', 'case'], ['not a case'], [1]):
            with self.assertRaises(ValueError):
                m.judge('', manifest)


if __name__ == '__main__':
    unittest.main()
