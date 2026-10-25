#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 John Knipper
"""Judge the complete native functional case manifest and a bounded runtime receipt."""
import argparse
import hashlib
import json
from pathlib import Path
import re

CASE = re.compile(r'FERAIL_FUNCTIONAL case=([a-z0-9_.-]+) result=(PASS|FAIL)(?: detail=.*)?')
FINAL = re.compile(r'FERAIL_FUNCTIONAL result=(PASS|FAIL) passed=(\d+) failed=(\d+)')


def judge(text, expected):
    problems, cases, finals, capabilities = [], [], [], []
    if (not isinstance(expected, list) or not expected or
            any(not isinstance(name, str) or not re.fullmatch('[a-z0-9_.-]+', name) for name in expected) or
            len(expected) != len(set(expected))):
        raise ValueError('case manifest must contain unique ASCII case identifiers')
    for line in text.splitlines():
        case, final = CASE.fullmatch(line), FINAL.fullmatch(line)
        if case:
            cases.append({'case': case[1], 'result': case[2]})
        elif final:
            finals.append((final[1], int(final[2]), int(final[3])))
        elif line.startswith('FERAIL_FUNCTIONAL '):
            problems.append('refused-or-malformed-record')
        elif line.startswith('FERAIL_CAPABILITY '):
            capabilities.append(line)
    names = [item['case'] for item in cases]
    if len(names) != len(set(names)):
        problems.append('duplicate-case')
    problems.extend('missing-case:' + name for name in sorted(set(expected) - set(names)))
    if set(names) - set(expected):
        problems.append('unexpected-case')
    if any(item['result'] != 'PASS' for item in cases):
        problems.append('case-failed')
    if finals != [('PASS', len(expected), 0)]:
        problems.append('invalid-final-result')
    return {'result': 'FAIL' if problems else 'PASS', 'problems': problems,
            'cases': cases, 'capabilities': capabilities}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--runtime-verdict', type=Path, required=True)
    parser.add_argument('--cases', type=Path, default=Path(__file__).resolve().parents[1] /
                        'crates/ferail-fs-native/examples/aros-functional-cases.json')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    raw = args.log.read_bytes()
    result = judge(raw.decode('latin1'), json.loads(args.cases.read_text()))
    runtime = json.loads(args.runtime_verdict.read_text())
    if runtime.get('result') != 'PASS':
        result['result'] = 'FAIL'
        result['problems'].append('runtime-gate-failed')
    result.update(scope='production filesystem/archive contracts; not GUI or native hardware acceptance',
                  log_sha256=hashlib.sha256(raw).hexdigest(),
                  case_manifest_sha256=hashlib.sha256(args.cases.read_bytes()).hexdigest(),
                  runtime_verdict_sha256=hashlib.sha256(args.runtime_verdict.read_bytes()).hexdigest())
    with args.output.open('x') as stream:
        json.dump(result, stream, indent=2)
        stream.write('\n')
    print(json.dumps({'result': result['result'], 'cases': len(result['cases']),
                      'problems': result['problems']}))
    return 0 if result['result'] == 'PASS' else 1


if __name__ == '__main__':
    raise SystemExit(main())
