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
parser.add_argument('--execution', action='store_true', help='Run the iteration 3C Session + Execution contract scenarios')
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
    with urllib.request.urlopen(request, timeout=35) as response:
        result = json.load(response)
    # Each call has a separate evidence file, including concurrent calls.
    (run/(uuid.uuid4().hex+'.json')).write_text(json.dumps({'method':method,'params':params,'response':result}, ensure_ascii=False), encoding='utf-8')
    if 'error' in result:
        raise RuntimeError(result['error'])
    return result['result']

def get(sid):
    return rpc('session.get', {'session_id':sid})

def command(sid, text):
    return rpc('debug.eval', {'session_id':sid,'command':text,'wait_ms':30000})

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
    result = rpc('debug.session.create', {'project_root':str(run),'target':target,'wait_ms':30000})
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

def run_execution_cases():
    def finish(eid):
        deadline = time.monotonic()+12
        value = rpc('execution.get', {'execution_id':eid})
        while not value['output_complete']:
            assert time.monotonic()<deadline, value
            assert not value['output_error'], value
            value = rpc('execution.get', {'execution_id':eid,'after_seq':value['sequence'],'wait_ms':1000})
        return value

    def execute(sid, text, wait_ms=1000, **extra):
        return rpc('session.exec', {'session_id':sid,'command':text,'wait_ms':wait_ms,**extra})

    def read_all(eid, cursor=None):
        chunks = []
        for _ in range(2000):
            value = rpc('execution.read', {'execution_id':eid,'cursor':cursor,'max_bytes':65536})
            assert not value['gap'], value
            chunks += [item['text'] for item in value['items'] if item['kind']=='text']
            cursor = value['next_cursor']
            if value['complete']:
                return ''.join(chunks),cursor
            assert value['has_more'], value
        raise AssertionError('output did not finish')

    opened = rpc('session.open', {'project_root':str(run),'backend':'dbgeng','target':{'kind':'launch','executable':str(fixture/'probe_target.exe'),'args':['loop']},'wait_ms':0})
    sid = opened['session_id']
    sessions.append(sid)
    assert opened['wait_expired']
    finish(opened['execution_id'])
    state = get(sid)
    target = process_handle(state['target']['process'])
    worker_id = state['worker']['process']['pid']
    execute(sid, '.sympath '+fixture.as_posix())
    loaded = execute(sid, '.load "'+(fixture/'probe_extension.dll').as_posix()+'"')
    assert 'failed' not in (loaded['output']['preview'] or '').lower(), loaded
    passed('session.open wait=0 retains opening execution and session IDs')

    started = time.monotonic()
    result = execute(sid, '!probe_extension.streaming', 20, client_request_id='stream-once',expected_service_instance=opened['service_instance'])
    assert time.monotonic()-started<1, result
    assert result['wait_expired'], result
    eid = result['execution_id']
    retry = execute(sid, '!probe_extension.streaming', 0, client_request_id='stream-once',expected_service_instance=opened['service_instance'])
    assert retry['execution_id']==eid
    rejects(lambda:execute(sid, '.echo busy', 0))
    rejects(lambda:execute(sid, '.echo conflict', 0,client_request_id='stream-once',expected_service_instance=opened['service_instance']))
    rejects(lambda:execute(sid, '!probe_extension.streaming', 0,client_request_id='stream-once',expected_service_instance='old-instance'))
    wait(sid,lambda s:s['engine']['state']=='executing')
    time.sleep(.05)
    partial = rpc('execution.read', {'execution_id':eid,'max_bytes':8192})
    first = ''.join(item['text'] for item in partial['items'] if item['kind']=='text')
    assert 'stream:begin' in first and not partial['complete'], partial
    final = finish(eid)
    assert final['status']=='success'
    assert final['execution_context']['context']['source']=='engine_owner_at_start', final
    rest,cursor = read_all(eid,partial['next_cursor'])
    assert first+rest=='stream:begin\nstream:end\n', (first,rest)
    assert get(sid)['worker']['process']['pid']==worker_id
    passed('bounded wait, busy, deduplication, actual start context, live .part to final cursor')

    # The HTTP client disappears after submitting a command; a keyed retry finds it.
    import http.client
    connection = http.client.HTTPConnection('127.0.0.1',port,timeout=2)
    payload={'jsonrpc':'2.0','id':'lost-response','method':'session.exec','params':{'session_id':sid,'command':'!probe_extension.streaming','wait_ms':1000,'client_request_id':'disconnected','expected_service_instance':opened['service_instance']}}
    connection.request('POST','/rpc',json.dumps(payload),{'Content-Type':'application/json'})
    connection.close()
    time.sleep(.15)
    recovered = execute(sid,'!probe_extension.streaming',0,client_request_id='disconnected',expected_service_instance=opened['service_instance'])
    assert finish(recovered['execution_id'])['status']=='success'
    assert get(sid)['worker']['process']['pid']==worker_id
    passed('client disconnect does not own the execution lifetime; keyed retry recovers result')

    before = get(sid)
    begin = time.monotonic()
    unchanged = rpc('session.get',{'session_id':sid,'after_seq':before['sequence'],'wait_ms':80})
    assert time.monotonic()-begin>=.06
    assert unchanged['sequence']==before['sequence']
    result = execute(sid,'g')
    assert finish(result['execution_id'])['status']=='success'
    running_state = wait(sid,lambda s:s['engine']['state']=='waiting_for_event')
    rejects(lambda:execute(sid,'.echo running',0))
    interrupt_result = rpc('tools/call',{'name':'session.interrupt','arguments':{'session_id':sid,'expected_run_epoch':running_state['engine']['run_epoch'],'wait_ms':1000}},endpoint='mcp')
    interrupted = json.loads(interrupt_result['content'][0]['text'])
    assert interrupted['accepted'], interrupted
    wait(sid,lambda s:s['engine']['state']=='idle' and s['target_state']['state']=='stopped')
    assert get(sid)['sequence']>before['sequence']
    passed('session state long observation, target-running admission, MCP interrupt')

    noisy = execute(sid,'.while (1) { .echo '+('output-'*32)+' }',0)
    wait(sid,lambda s:s['engine']['state']=='executing')
    time.sleep(.15)
    canceled = rpc('execution.cancel',{'execution_id':noisy['execution_id'],'wait_ms':1000})
    assert canceled['control_execution']['related_execution_id']==noisy['execution_id']
    final = finish(noisy['execution_id'])
    assert final['status']=='canceled',final
    output,_ = read_all(noisy['execution_id'])
    assert len(output)>65536
    assert finish(execute(sid,'.echo after-cancel')['execution_id'])['status']=='success'
    passed('large live output, independent cancellation, terminal partial output reads')

    batch = rpc('debug.eval_steps',{'session_id':sid,'commands':['g','.echo must-not-run'],'continue_on_error':True,'wait_ms':30000})
    assert batch['unexecuted_steps']==[1] and batch['stop_reason']=='target_running',batch
    child=rpc('execution.get',{'execution_id':batch['steps'][0]['operation_id']})
    assert child['parent_execution_id']==batch['operation_id'],child
    state=wait(sid,lambda s:s['engine']['state']=='waiting_for_event')
    rpc('session.interrupt',{'session_id':sid,'expected_run_epoch':state['engine']['run_epoch']})
    idle(sid)
    batch=rpc('debug.eval_steps',{'session_id':sid,'commands':['!probe_extension.cooperative','.echo must-not-run'],'continue_on_error':True,'wait_ms':0})
    wait(sid,lambda s:s['engine']['state']=='executing')
    time.sleep(.1)
    rpc('execution.cancel',{'execution_id':batch['execution_id']})
    ended=finish(batch['execution_id'])
    assert ended['status']=='canceled' and ended['batch_result']['unexecuted_steps']==[1],ended
    passed('batch child lineage and stop after target run or cancellation despite continue_on_error')

    long = execute(sid,'!probe_extension.streaming',0)
    wait(sid,lambda s:s['engine']['state']=='executing')
    closing = rpc('session.close',{'session_id':sid,'wait_ms':0})
    repeated = rpc('session.close',{'session_id':sid,'wait_ms':0})
    assert closing['execution_id']==repeated['execution_id']
    assert closing['session']['lifecycle']=='closing'
    rejects(lambda:execute(sid,'.echo after-close',0))
    assert finish(long['execution_id'])['status']=='success'
    assert finish(closing['execution_id'])['status']=='success'
    sessions.remove(sid)
    assert kernel.WaitForSingleObject(target,0)==258
    output,_ = read_all(long['execution_id'])
    assert output=='stream:begin\nstream:end\n'
    rejects(lambda:rpc('session.read',{'session_id':sid,'cursor':cursor}))
    canceled = rpc('execution.cancel',{'execution_id':long['execution_id']})
    assert canceled['code']=='already_finished' and canceled['execution']['status']=='success'
    passed('close coordination/idempotence, preserved target, post-close output, already-finished cancel')

    page = rpc('execution.list',{'project_root':str(run),'session_id':sid,'limit':2})
    assert len(page['executions'])==2 and page['has_more']
    next_page = rpc('execution.list',{'project_root':str(run),'session_id':sid,'limit':2,'cursor':page['next_cursor']})
    assert page['executions'][0]['execution_id']!=next_page['executions'][0]['execution_id']
    artifacts = rpc('artifact.list',{'project_root':str(run)})['artifacts']
    produced = [a for a in artifacts if a.get('producer_execution_id')==long['execution_id']]
    assert produced and all(a['producer_session_id']==sid for a in produced)
    passed('paged execution history and exact session/execution artifact provenance')

    # Reattach to the exact fixture process preserved by normal close.
    target_pid=state['target']['process']['pid']
    for mode in ['noninvasive','invasive']:
        opened_attach=rpc('session.open',{'project_root':str(run),'target':{'kind':'attach','pid':target_pid,'mode':mode},'wait_ms':30000})
        attached=opened_attach['session_id'];sessions.append(attached)
        assert opened_attach['execution']['status']=='success',opened_attach
        observed=get(attached)
        assert observed['context']['target_control']==(mode=='invasive'),observed
        if mode=='noninvasive':
            denied=rpc('session.interrupt',{'session_id':attached})
            assert not denied['accepted'] and denied['code']=='unsupported',denied
        if mode=='invasive':
            execute(attached,'g')
            observed=wait(attached,lambda s:s['engine']['state']=='waiting_for_event')
            rpc('session.interrupt',{'session_id':attached,'expected_run_epoch':observed['engine']['run_epoch']})
            idle(attached)
        assert finish(rpc('session.close',{'session_id':attached})['execution_id'])['status']=='success'
        sessions.remove(attached)
        assert kernel.WaitForSingleObject(target,0)==258
    passed('explicit invasive attach runs and interrupts; default observation attach preserves target')
    for cause in ['kill','crash']:
        opened_attach=rpc('session.open',{'project_root':str(run),'target':{'kind':'attach','pid':target_pid,'mode':'invasive'},'wait_ms':30000})
        attached=opened_attach['session_id'];sessions.append(attached)
        assert opened_attach['execution']['status']=='success',opened_attach
        if cause=='kill':
            finish(rpc('session.kill',{'session_id':attached})['execution_id'])
        else:
            worker=process_handle(get(attached)['worker']['process'])
            assert kernel.TerminateProcess(worker,73)
        wait(attached,lambda s:s['lifecycle']=='failed' and s['cleanup']['complete'])
        assert kernel.WaitForSingleObject(target,100)==258,'external invasive target died on '+cause
        sessions.remove(attached)
    passed('invasive attached external target survives explicit kill and worker crash')


    streams=rpc('session.read',{'session_id':sid,'max_bytes':65536})
    assert any(item['source']=='engine_events' for item in streams['items'] if item['kind']=='text'),streams
    async_artifacts=[a for a in rpc('artifact.list',{'project_root':str(run)})['artifacts'] if a['kind']=='debug.session_stream']
    assert async_artifacts and all(a['producer_session_id'] and a['producer_execution_id'] is None for a in async_artifacts)
    passed('session asynchronous streams retain session-only producer attribution')

    if args.ttd:
        replay=rpc('session.open',{'project_root':str(run),'target':{'kind':'file','path':str(args.ttd.resolve())},'wait_ms':0})
        ttd=replay['session_id'];sessions.append(ttd)
        assert finish(replay['execution_id'])['status']=='success'
        assert get(ttd)['target_state']['state']=='offline'
        assert finish(execute(ttd,'!tt 0',0)['execution_id'])['status']=='success'
        assert finish(execute(ttd,'g',0)['execution_id'])['status']=='success'
        idle(ttd)
        assert get(ttd)['target_state']['state']=='offline'
        query=execute(ttd,'.echo replay-result')
        assert 'replay-result' in read_all(query['execution_id'])[0]
        assert finish(rpc('session.close',{'session_id':ttd})['execution_id'])['status']=='success'
        sessions.remove(ttd)
        passed('TTD replay through finite-wait Session + Execution with offline target state')

    if args.dump:
        # Fixture registration models an imported input, without inventing a producer.
        import shutil
        dest = run/'dbgatlas/artifacts/inputs/fixture.dmp'
        dest.parent.mkdir(parents=True,exist_ok=True)
        shutil.copyfile(args.dump,dest)
        reference={'id':'fixture-external-dump'}
        metadata={'artifact_id':reference,'kind':'dump','relative_path':'artifacts/inputs/fixture.dmp','created_at':{'unix_millis':int(time.time()*1000)},'operation_id':None,'producer_session_id':None,'producer_execution_id':None,'byte_len':dest.stat().st_size,'description':'synthetic imported fixture'}
        with (run/'dbgatlas/artifacts/artifacts.jsonl').open('a',encoding='utf-8') as output:
            output.write(json.dumps(metadata)+'\n')
        reused = rpc('session.open',{'project_root':str(run),'target':{'kind':'file','artifact_ref':reference},'wait_ms':30000})
        other = reused['session_id']
        sessions.append(other)
        assert reused['execution']['status']=='success',reused
        assert reused['session']['input_artifacts']==[reference]
        assert finish(execute(other,'.echo artifact-input')['execution_id'])['status']=='success'
        assert finish(rpc('session.close',{'session_id':other})['execution_id'])['status']=='success'
        sessions.remove(other)
        registered = [a for a in rpc('artifact.list',{'project_root':str(run)})['artifacts'] if a['artifact_id']==reference]
        assert len(registered)==1 and registered[0]['producer_session_id'] is None
        passed('same-workspace Artifact input opens a real Dump without changing its producer')

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
    if args.execution:
        run_execution_cases()
    else:
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
        assert rpc('operation.cancel', {'operation_id':operation})['code']=='already_finished'
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
