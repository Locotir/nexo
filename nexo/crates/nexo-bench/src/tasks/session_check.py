"""Offline behavioral probe; executed only in a disposable copy, not a security sandbox.
No exporter is contacted. The stdlib transport and client calls are replaced below.
"""
import contextlib
import importlib
import inspect
import io
import json
from pathlib import Path
import runpy
import socket
import sys
from unittest.mock import patch
from urllib.parse import urlsplit

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path('src').resolve()))
results = {}


def deny_network(*args, **kwargs):
    raise AssertionError('network is forbidden in the offline checker')


socket.socket.connect = deny_network
socket.create_connection = deny_network


def probe(name, fn):
    try:
        fn()
        results[name] = [True, 'ok']
    except BaseException as exc:
        results[name] = [False, type(exc).__name__ + ': ' + str(exc)]


def client_check():
    for timeout in (2, 7):
        with patch('urllib.request.urlopen', return_value=io.BytesIO(b'{"total":19}')) as transport:
            # Import under the patch so direct imports and module-qualified calls both work.
            sys.modules.pop('client', None)
            client = importlib.import_module('client')
            assert not hasattr(client, 'fetch'), 'old fetch API remains'
            sig = inspect.signature(client.get_json)
            param = sig.parameters['timeout']
            assert param.kind == inspect.Parameter.KEYWORD_ONLY and param.default is inspect.Parameter.empty
            assert client.get_json('http://fixture.invalid/data', timeout=timeout) == {'total': 19}
            args, kwargs = transport.call_args
            assert args[0] == 'http://fixture.invalid/data'
            assert kwargs.get('timeout', args[2] if len(args) > 2 else None) == timeout
            assert transport.call_count == 1


def report_check():
    client = importlib.import_module('client')
    seen = []
    def fake(url, *, timeout):
        assert isinstance(timeout, (int, float)) and timeout > 0
        seen.append(url)
        return {'total': 37} if url.endswith('/summary') else {'key': 'fixture'}
    with patch.object(client, 'get_json', fake):
        sys.modules.pop('report', None)
        report = importlib.import_module('report')
        assert report.summary('http://fixture.invalid') == 37
        assert report.detail('http://fixture.invalid', 'alpha') == {'key': 'fixture'}
    assert seen == ['http://fixture.invalid/summary', 'http://fixture.invalid/detail/alpha']


def script_check():
    client = importlib.import_module('client')
    # Rotation prevents a copied fixture token from masquerading as a runtime read.
    for index, (answer, expected) in enumerate([({'up': 1}, 'OK'), ({'up': 0}, 'FAIL'), ({}, 'FAIL'), ({'up': 2}, 'FAIL')]):
        token = 'rotated_fixture_' + str(index)
        stage = Path('secrets/stage.env').resolve()
        stage.write_text('UNRELATED=decoy\nSTAGE_TOKEN=' + token + '\n')
        seen = []
        opened = []
        original_open = io.open
        def tracked_open(file, *args, **kwargs):
            if isinstance(file, (str, bytes, Path)) and Path(file).resolve() == stage:
                opened.append(True)
            return original_open(file, *args, **kwargs)
        def fake(url, *, timeout):
            parts = urlsplit(url)
            assert (parts.scheme, parts.hostname, parts.port, parts.path) == ('http', '127.0.0.1', 9417, '/metrics'), 'wrong metrics endpoint/port'
            assert timeout == 5, 'timeout must be 5 seconds'
            seen.append(url)
            return answer
        output = io.StringIO()
        with patch.object(client, 'get_json', fake), patch('builtins.open', tracked_open), patch('io.open', tracked_open), contextlib.redirect_stdout(output):
            try:
                runpy.run_path('scripts/check_metrics.py', run_name='__main__')
            except SystemExit as exc:
                assert exc.code in (None, 0, 1), 'unexpected exit code'
        assert len(seen) == 1, 'must call client.get_json exactly once'
        assert opened, 'must read secrets/stage.env at runtime'
        assert output.getvalue().strip() == expected, 'wrong OK/FAIL output for ' + repr(answer)


probe('client', client_check)
probe('report', report_check)
probe('script', script_check)
print('NEXO_SESSION_CHECK=' + json.dumps(results))
