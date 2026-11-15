"""Real Windows worker regression; build the fixture with MSVC before running.

cmake -S tests/fixtures/interaction -B temp/interaction-fixture -A x64
cmake --build temp/interaction-fixture --config Release
python script/test-worker-interaction.py --fixture temp/interaction-fixture/Release

All service logs, raw RPC results and synthetic workspaces stay under temp/.
Optional --dump/--ttd paths are opened read-only by the debugger.
"""
import argparse
import concurrent.futures
import ctypes
from ctypes import wintypes
import json
import os
import pathlib
import socket
import subprocess
import time
import urllib.request
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--fixture', type=pathlib.Path, required=True)
parser.add_argument('--binary', type=pathlib.Path, default=ROOT/'target/debug/dbgatlas.exe')
parser.add_argument('--dump', type=pathlib.Path)
parser.add_argument('--ttd', type=pathlib.Path)
parser.add_argument('--runtime', type=pathlib.Path, help='Explicit DbgEng directory; requires cargo build -p dbgatlas-service --example interaction-host')
args = parser.parse_args()
fixture = args.fixture.resolve()
run = ROOT/'temp'/('worker-interaction-'+uuid.uuid4().hex[:8])
run.mkdir(parents=True)
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel.OpenProcess.restype = wintypes.HANDLE
kernel.CloseHandle.argtypes = [wintypes.HANDLE]
kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
kernel.GetProcessTimes.argtypes = [wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
handles, sessions, checks = [], [], []

def process_handle(identity):
    """Retain the exact process instance; never clean up an unverified/reused PID."""
    handle = kernel.OpenProcess(0x100000 | 0x1000 | 1, False, identity['pid'])
    assert handle, ctypes.get_last_error()
    times = [wintypes.FILETIME() for _ in range(4)]
    assert kernel.GetProcessTimes(handle, *[ctypes.byref(t) for t in times])
    actual = (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime
    if actual != int(identity['creation_time_100ns']):
        kernel.CloseHandle(handle)
        raise AssertionError('process instance changed')
    handles.append(handle)
    return handle

with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    port = listener.getsockname()[1]
log = (run/'service.log').open('w')
environment = dict(os.environ, _NT_SYMBOL_PATH=str(fixture), _NT_ALT_SYMBOL_PATH=str(fixture))
invocation = [str(args.binary.resolve()), 'service', 'run', '--bind', f'127.0.0.1:{port}']
if args.runtime:
    invocation = [str(args.binary.resolve().parent/'examples/interaction-host.exe'), '--bind', f'127.0.0.1:{port}', '--worker', str(args.binary.resolve().parent/'dbgatlas-worker.exe'), '--dbgeng', str(args.runtime.resolve())]
service = subprocess.Popen(invocation, env=environment, stdout=log, stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW)
pool = concurrent.futures.ThreadPoolExecutor(4)

def rpc(method, params=None, endpoint='rpc'):
    request = urllib.request.Request(f'http://127.0.0.1:{port}/{endpoint}', json.dumps({'jsonrpc':'2.0','id':uuid.uuid4().hex,'method':method,'params':params or {}}).encode(), {'Content-Type':'application/json'})
    with urllib.request.urlopen(request, timeout=20) as response:
        result = json.load(response)
    # Each call has a separate evidence file, including concurrent calls.
    (run/(uuid.uuid4().hex+'.json')).write_text(json.dumps({'method':method,'params':params,'response':result}, ensure_ascii=False), encoding='utf-8')
    if 'error' in result:
        raise RuntimeError(result['error'])
    return result['result']

def get(sid):
    return rpc('session.get', {'session_id':sid})

def command(sid, text):
    return rpc('debug.eval', {'session_id':sid,'command':text})

def wait(sid, predicate, seconds=8):
    deadline = time.monotonic()+seconds
    while time.monotonic() < deadline:
        state = get(sid)
        if predicate(state):
            return state
        time.sleep(.03)
    raise AssertionError(f'state did not converge: {state}')

def idle(sid):
    return wait(sid, lambda s:s['context']['engine']['state']=='idle')

def create(target):
    result = rpc('debug.session.create', {'project_root':str(run),'target':target})
    sid = result['session_id']
    sessions.append(sid)
    state = get(sid)
    handle = process_handle(state['target']['process']) if target['kind']=='launch' else None
    command(sid, '.sympath '+str(fixture))
    return sid, handle

def launch(loop=True):
    return create({'kind':'launch','executable':str(fixture/'probe_target.exe'),'args':['loop'] if loop else []})

def running(sid):
    command(sid, 'g')
    return wait(sid, lambda s:s['context']['engine']['state']=='waiting_for_event')

def interrupt(sid, epoch):
    result = rpc('tools/call', {'name':'session.interrupt','arguments':{'session_id':sid,'expected_epoch':epoch}}, endpoint='mcp')
    if result.get('isError'):
        raise RuntimeError(result)
    return json.loads(result['content'][0]['text'])

def close(sid):
    rpc('debug.session.close', {'session_id':sid})
    sessions.remove(sid)

def rejects(call):
    try:
        call()
    except RuntimeError:
        return
    raise AssertionError('request should have been rejected')

def passed(name):
    checks.append(name)
    print('PASS', name, flush=True)

try:
    for _ in range(100):
        try:
            rpc('service.health')
            break
        except Exception:
            time.sleep(.05)
    rpc('initialize', {'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'worker-regression','version':'1'}}, endpoint='mcp')
    inventory = rpc('tools/list', endpoint='mcp')
    assert any(tool['name']=='session.interrupt' for tool in inventory['tools'])
    passed('development MCP discovery; interrupts below use MCP tools/call')
    sid, target = launch()
    state = running(sid)
    epoch = state['context']['engine']['epoch']
    worker_pid = state['worker']['process']['pid']
    assert not interrupt(sid, epoch+100)['accepted']
    rejects(lambda:command(sid, '.echo busy'))
    assert get(sid)['worker']['process']['pid']==worker_pid
    assert interrupt(sid, epoch)['accepted']
    assert idle(sid)['target_state']['state']=='stopped'
    assert 'resumed-query' in command(sid, '.echo resumed-query')['result']
    passed('launch, event wait, busy, ACTIVE interrupt, reusable session')
    state = running(sid)
    assert not interrupt(sid, epoch)['accepted']
    assert get(sid)['target_state']['state']=='running'
    close(sid)
    assert kernel.WaitForSingleObject(target, 0)==258
    passed('stale epoch rejected; EXIT/close preserves running target')
    sid,_ = create({'kind':'attach','pid':state['target']['process']['pid']})
    state = get(sid)
    assert state['target_state']['state']=='unknown'
    assert state['context']['target_control'] is False
    assert 'observer' in command(sid, '.echo observer')['result']
    close(sid)
    assert kernel.WaitForSingleObject(target, 0)==258
    passed('legacy noninvasive attach remains observation-only and preserves target')

    sid, target = launch(False)
    command(sid, 'g')
    state = wait(sid, lambda s:s['context'].get('last_event',{}).get('kind')==2 and s['context']['engine']['state']=='idle')
    assert state['target_state']['state']=='stopped'
    command(sid, 'g')
    wait(sid, lambda s:s['target_state']['state']=='exited')
    assert kernel.WaitForSingleObject(target, 2000)==0
    close(sid)
    passed('DebugBreak and process exit consumed without another query command')

    sid, target = launch()
    pending = pool.submit(command, sid, '.echo before-cancel; .while (1) { r @$t0 = @$t0 + 1 }')
    state = wait(sid, lambda s:s['context']['engine']['state']=='executing')
    operation = state['context']['engine']['active_operation_id']
    time.sleep(.1)
    assert rpc('operation.cancel', {'operation_id':operation})['cancellation_requested']
    rejects(lambda:pending.result(8))
    idle(sid)
    final = rpc('operation.get', {'operation_id':operation})
    assert final['status']=='canceled', final
    assert any('before-cancel' in p.read_text(errors='replace') for p in run.rglob('*.part'))
    rejects(lambda:rpc('operation.cancel', {'operation_id':operation}))
    assert 'after-cancel' in command(sid, '.echo after-cancel')['result']
    passed('PASSIVE cancels native loop, preserves raw output, no residual cancellation')

    pending = pool.submit(command, sid, '.while (1) { .echo '+('output-'*32)+' }')
    state = wait(sid, lambda s:s['context']['engine']['state']=='executing')
    operation = state['context']['engine']['active_operation_id']
    time.sleep(.2)
    assert rpc('operation.cancel', {'operation_id':operation})['cancellation_requested']
    rejects(lambda:pending.result(8))
    idle(sid)
    assert any(p.stat().st_size>65536 for p in run.rglob('*.part'))
    assert 'after-output' in command(sid, '.echo after-output')['result']
    passed('large raw output stays off the control pipe and remains recoverable')

    loaded = command(sid, '.load "'+(fixture/'probe_extension.dll').as_posix()+'"')
    assert 'failed' not in loaded['result'].lower(), loaded
    for name in ['cooperative','uncooperative']:
        pending = pool.submit(command, sid, '!probe_extension.'+name)
        state = wait(sid, lambda s:s['context']['engine']['state']=='executing')
        operation = state['context']['engine']['active_operation_id']
        time.sleep(.1)
        rejects(lambda:command(sid, '.echo concurrent'))
        assert rpc('operation.cancel', {'operation_id':operation})['cancellation_requested']
        result = pending.result(8)
        assert result['operation_status']=='success', result
        expected = name+(':interrupted' if name=='cooperative' else ':finished')
        assert expected in result['result'], result
        assert get(sid)['worker']['process']['pid']==state['worker']['process']['pid']
        assert 'next' in command(sid, '.echo next')['result']
        passed(name+' extension: delivery does not overwrite native success')

    # Both operations may win admission; neither may deliver to a successor epoch.
    state = running(sid)
    first = pool.submit(interrupt, sid, state['context']['engine']['epoch'])
    second = pool.submit(close, sid)
    for future in [first,second]:
        try:
            future.result(8)
        except RuntimeError:
            pass
    if sid in sessions:
        close(sid)
    assert kernel.WaitForSingleObject(target, 0)==258
    passed('interrupt/close race preserves target and completes')

    sid, target = launch()
    state = running(sid)
    worker = process_handle(state['worker']['process'])
    assert kernel.TerminateProcess(worker, 91)
    wait(sid, lambda s:s['connection']=='disconnected')
    assert kernel.WaitForSingleObject(target, 5000)==0
    sessions.remove(sid)
    passed('worker death observed; verified owned target cleaned up')

    for kind,path in [('dump',args.dump),('ttd',args.ttd)]:
        if not path:
            continue
        sid,_ = create({'kind':'file','path':str(path.resolve())})
        assert get(sid)['target_state']['state']=='offline'
        assert 'offline-query' in command(sid, '.echo offline-query')['result']
        if kind=='ttd':
            command(sid, '!tt 0')
            command(sid, 'g')
            idle(sid)
            assert get(sid)['target_state']['state']=='offline'
        close(sid)
        passed(kind+' offline query/replay')
    events = list(run.rglob('engine-events.jsonl'))
    assert events and any('"kind":3' in p.read_text() for p in events)
    assert list(run.rglob('engine-results.jsonl'))
    passed('durable native event and HRESULT evidence')
finally:
    for sid in sessions:
        try:
            rpc('debug.session.kill', {'session_id':sid})
        except Exception:
            pass
    for handle in handles:
        if kernel.WaitForSingleObject(handle, 0)==258:
            kernel.TerminateProcess(handle, 0)
            kernel.WaitForSingleObject(handle, 5000)
        kernel.CloseHandle(handle)
    service.terminate()
    service.wait(8)
    pool.shutdown(wait=True)
    log.close()
    (run/'summary.json').write_text(json.dumps({'passed':checks},indent=2))
    print('Evidence:',run,flush=True)
