"""Iteration 3D real CLI/MCP regression; all scratch stays in repo temp.

Build the MSVC fixture and CLI/worker first. --codex optionally exercises the
installed Codex app-server MCP client with an isolated ephemeral test context;
no model turn or user configuration change is performed.
"""
import argparse, ctypes, http.server, json, os, pathlib, queue, socket
import subprocess, threading, time, urllib.request, uuid
from ctypes import wintypes

ROOT=pathlib.Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=pathlib.Path,default=ROOT/'target/debug/dbgatlas.exe')
parser.add_argument('--fixture',type=pathlib.Path,required=True)
parser.add_argument('--codex',type=pathlib.Path)
parser.add_argument('--baseline',type=pathlib.Path,help='A previous worker-interaction evidence directory for tools/list size comparison')
args=parser.parse_args();binary=args.binary.resolve();fixture=args.fixture.resolve()
run=ROOT/'temp'/('client-interaction-'+uuid.uuid4().hex[:8]);run.mkdir(parents=True)
checks=[];sessions=[];service=None;client=None;target_handle=None;relay=None

def save(name,value):
    (run/(name+'.json')).write_text(json.dumps(value,ensure_ascii=False,indent=2),encoding='utf-8')
def passed(name):
    checks.append(name);print('PASS',name,flush=True)
def rpc(method,params=None,version=None):
    endpoint='mcp' if version else 'rpc'
    headers={'Content-Type':'application/json','Accept':'application/json, text/event-stream'}
    if version:headers['MCP-Protocol-Version']=version
    body={'jsonrpc':'2.0','id':uuid.uuid4().hex,'method':method,'params':params or {}}
    request=urllib.request.Request(f'http://127.0.0.1:{port}/{endpoint}',json.dumps(body).encode(),headers)
    with urllib.request.urlopen(request,timeout=40) as response:result=json.load(response)
    save(uuid.uuid4().hex,{'request':body,'response':result,'version':version})
    assert 'error' not in result,result
    return result['result']
def mcp(name,arguments,version='2025-06-18'):
    result=rpc('tools/call',{'name':name,'arguments':arguments},version)
    assert not result.get('isError'),result
    return result.get('structuredContent') or json.loads(result['content'][0]['text'])
def cli(*arguments,fail=False):
    arguments=list(arguments);split=arguments.index('--') if '--' in arguments else len(arguments)
    arguments[split:split]=['--endpoint',f'127.0.0.1:{port}']
    result=subprocess.run([str(binary),'--json',*arguments],capture_output=True,text=True,encoding='utf-8',timeout=40,creationflags=subprocess.CREATE_NO_WINDOW)
    save(uuid.uuid4().hex,{'cli':arguments,'code':result.returncode,'stdout':result.stdout,'stderr':result.stderr})
    assert bool(result.returncode)==fail,result
    response=json.loads(result.stdout)
    return response if fail else response['result']
def ref(value):return value['id'] if isinstance(value,dict) else value
def finished(eid):
    value=cli('execution','get',ref(eid),'--wait-ms','30000')
    assert value['finalized'],value
    return value
def wait(sid,state):
    deadline=time.monotonic()+10
    while time.monotonic()<deadline:
        value=rpc('session.get',{'session_id':sid})
        if value['engine']['state']==state:return value
        time.sleep(.03)
    raise AssertionError(value)

kernel=ctypes.WinDLL('kernel32',use_last_error=True)
kernel.OpenProcess.argtypes=[wintypes.DWORD,wintypes.BOOL,wintypes.DWORD];kernel.OpenProcess.restype=wintypes.HANDLE
kernel.GetProcessTimes.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
kernel.TerminateProcess.argtypes=[wintypes.HANDLE,wintypes.UINT]
kernel.WaitForSingleObject.argtypes=[wintypes.HANDLE,wintypes.DWORD]
kernel.CloseHandle.argtypes=[wintypes.HANDLE]

def codex_client(sid):
    global client,relay
    # Relay records the actual client's initialization, version headers and calls.
    class Relay(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body=self.rfile.read(int(self.headers.get('Content-Length','0')))
            request=urllib.request.Request(f'http://127.0.0.1:{port}/mcp',body,{key:value for key,value in self.headers.items() if key.lower() not in ['host','content-length']})
            with urllib.request.urlopen(request,timeout=40) as response:
                data=response.read();self.send_response(response.status)
                self.send_header('Content-Type',response.headers.get('Content-Type','application/json'))
                self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
            save('wire-'+uuid.uuid4().hex,{'headers':dict(self.headers),'request':json.loads(body),'response':json.loads(data) if data else None})
        def do_GET(self):self.send_error(405)
        def do_DELETE(self):self.send_error(405)
        def log_message(self,*args):pass
    relay=http.server.ThreadingHTTPServer(('127.0.0.1',0),Relay)
    threading.Thread(target=relay.serve_forever,daemon=True).start()
    probe_home=run/'codex-home';probe_home.mkdir()
    (probe_home/'config.toml').write_text('[mcp_servers.dbgatlas_probe]\nurl = "http://127.0.0.1:'+str(relay.server_port)+'/mcp"\n',encoding='utf-8')
    environment=dict(os.environ,CODEX_HOME=str(probe_home))
    client=subprocess.Popen([str(args.codex.resolve()),'app-server'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(run/'codex.stderr').open('w'),text=True,encoding='utf-8',env=environment,cwd=run,creationflags=subprocess.CREATE_NO_WINDOW)
    messages=queue.Queue()
    def receive():
        for line in client.stdout:
            value=json.loads(line);messages.put(value)
            with (run/'codex-wire.jsonl').open('a',encoding='utf-8') as output:output.write(json.dumps(value,ensure_ascii=False)+'\n')
    threading.Thread(target=receive,daemon=True).start()
    def call(method,params):
        rid=uuid.uuid4().hex
        client.stdin.write(json.dumps({'id':rid,'method':method,'params':params})+'\n');client.stdin.flush()
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            reply=messages.get(timeout=max(.1,deadline-time.monotonic()))
            if reply.get('id')==rid:
                assert 'error' not in reply,reply
                return reply['result']
        raise AssertionError('app-server request timed out')
    call('initialize',{'clientInfo':{'name':'dbgatlas_3d_test','version':'1'},'capabilities':{'experimentalApi':True}})
    client.stdin.write('{"method":"initialized","params":{}}\n');client.stdin.flush()
    thread=call('thread/start',{'cwd':str(run),'ephemeral':True,'approvalPolicy':'never','sandbox':'read-only'})
    tid=thread['thread']['id']
    inventory=call('mcpServerStatus/list',{'serverName':'dbgatlas_probe','threadId':tid})
    save('codex-inventory',inventory)
    observations=[]
    def tool(name,arguments):
        started=time.monotonic()
        result=call('mcpServer/tool/call',{'threadId':tid,'server':'dbgatlas_probe','tool':name,'arguments':arguments})
        observations.append({'tool':name,'elapsed_ms':round((time.monotonic()-started)*1000,2),'text_bytes':sum(len(item.get('text','').encode()) for item in result['content']),'result_bytes':len(json.dumps(result,separators=(',',':'),ensure_ascii=False).encode())})
        save('codex-call-metrics',observations)
        assert not result.get('isError'),result
        assert isinstance(result.get('structuredContent'),dict),result
        return result
    short=tool('session.exec',{'session_id':sid,'command':'.echo codex-client-marker','wait_ms':1000})
    assert short['structuredContent']['execution']['status']=='success',short
    assert 'codex-client-marker' in short['structuredContent']['output']['preview']
    assert 'codex-client-marker' not in short['content'][0]['text']
    slow=tool('session.exec',{'session_id':sid,'command':'!probe_extension.streaming','wait_ms':10})
    assert slow['structuredContent']['wait_expired'],slow
    eid=slow['structuredContent']['execution_id']
    observed=tool('execution.get',{'execution_id':eid,'wait_ms':30000})
    assert observed['structuredContent']['status']=='success'
    output=tool('execution.read',{'execution_id':eid})['structuredContent']
    assert output['complete'] and any('stream:end' in item.get('text','') for item in output['items'])
    tool('session.exec',{'session_id':sid,'command':'g','wait_ms':1000})
    state=wait(sid,'waiting_for_event')
    interrupted=tool('session.interrupt',{'session_id':sid,'expected_run_epoch':state['engine']['run_epoch']})
    assert interrupted['structuredContent']['accepted'];wait(sid,'idle')
    passed('actual Codex app-server client discovers tools and preserves structured short/long/read/interrupt results')

try:
    with socket.socket() as sock:sock.bind(('127.0.0.1',0));port=sock.getsockname()[1]
    service_log=(run/'service.log').open('w')
    environment=dict(os.environ,_NT_SYMBOL_PATH=str(fixture),_NT_ALT_SYMBOL_PATH=str(fixture))
    service=subprocess.Popen([str(binary),'service','run','--bind',f'127.0.0.1:{port}'],stdout=service_log,stderr=subprocess.STDOUT,env=environment,creationflags=subprocess.CREATE_NO_WINDOW)
    for _ in range(100):
        try:rpc('service.health');break
        except Exception:time.sleep(.05)
    for version in ['2024-11-05','2025-03-26','2025-06-18']:
        assert rpc('initialize',{'protocolVersion':version,'capabilities':{},'clientInfo':{'name':'3d-probe','version':'1'}},version)['protocolVersion']==version
        assert mcp('service.health',{},version)['status']=='ok'
    inventory=rpc('tools/list',{},'2025-06-18')['tools'];names=[tool['name'] for tool in inventory]
    assert 'session.exec' in names and 'debug.eval' not in names and 'execution.read' in names and 'operation.stream' not in names
    metrics={'tool_count':len(inventory),'tool_schema_bytes':len(json.dumps(inventory,separators=(',',':'),ensure_ascii=False).encode())}
    if args.baseline:
        for path in args.baseline.glob('*.json'):
            previous=json.loads(path.read_text(encoding='utf-8'))
            if previous.get('method')=='tools/list':
                old=previous['response']['result']['tools'];metrics.update(baseline_tool_count=len(old),baseline_tool_schema_bytes=len(json.dumps(old,separators=(',',':'),ensure_ascii=False).encode()));break
    save('inventory-metrics',metrics);passed('three MCP versions, legacy text fallback, canonical inventory and measured descriptor sizes')
    opened=cli('session','open','--project-root',str(run),'--launch',str(fixture/'probe_target.exe'),'--wait-ms','30000','--','loop')
    sid=opened['session_id'];sessions.append(sid)
    process=opened['session']['target']['process']
    target_handle=kernel.OpenProcess(0x100000|0x1000|1,False,process['pid']);assert target_handle
    times=[wintypes.FILETIME() for _ in range(4)];assert kernel.GetProcessTimes(target_handle,*[ctypes.byref(t) for t in times])
    assert (times[0].dwHighDateTime<<32|times[0].dwLowDateTime)==int(process['creation_time_100ns'])
    cli('session','help','execution','--session-id',ref(sid))
    cli('session','exec',ref(sid),'.sympath '+fixture.as_posix())
    cli('session','exec',ref(sid),'.load "'+(fixture/'probe_extension.dll').as_posix()+'"')
    short=cli('session','exec',ref(sid),'.echo cli-short')
    assert short['execution']['status']=='success'
    read=cli('execution','read',ref(short['execution_id']),'--max-bytes','64');assert any('cli-short' in item.get('text','') for item in read['items'])
    slow=cli('session','exec',ref(sid),'!probe_extension.streaming','--wait-ms','0','--client-request-id','cli-once','--expected-service-instance',opened['service_instance'])
    retry=cli('session','exec',ref(sid),'!probe_extension.streaming','--wait-ms','0','--client-request-id','cli-once','--expected-service-instance',opened['service_instance'])
    assert retry['execution_id']==slow['execution_id'];assert finished(slow['execution_id'])['status']=='success'
    cli('execution','list','--project-root',str(run),'--session-id',ref(sid),'--limit','2')
    error=cli('session','exec','missing-session','.echo forbidden',fail=True);assert 'error' in error
    passed('real CLI open/exec/get/read/list/help, finite wait, deduplication and JSON nonzero failures')
    if args.codex:codex_client(sid)
    noisy=cli('session','exec',ref(sid),'.while (1) { r @$t0 = @$t0 + 1 }','--wait-ms','0')
    wait(sid,'executing');time.sleep(.05)
    cancel=cli('execution','cancel',ref(noisy['execution_id']));assert cancel['control_execution']['status']=='success'
    assert finished(noisy['execution_id'])['status']=='canceled'
    cli('session','exec',ref(sid),'g');state=wait(sid,'waiting_for_event')
    cli('session','interrupt',ref(sid),'--expected-run-epoch',str(state['engine']['run_epoch']));wait(sid,'idle')
    cli('debug','eval',ref(sid),'.echo compatibility')
    close=cli('session','close',ref(sid),'--wait-ms','30000');assert close['session']['lifecycle']=='closed';sessions.remove(sid)
    cli('execution','read',ref(short['execution_id']));cli('session','read',ref(sid));assert kernel.WaitForSingleObject(target_handle,0)==258
    passed('CLI cancellation, target interruption, legacy alias, normal close and post-close reads')
finally:
    if client:
        client.stdin.close()
        try:client.wait(5)
        except subprocess.TimeoutExpired:client.terminate();client.wait(5)
    if relay:relay.shutdown();relay.server_close()
    for sid in sessions:
        try:rpc('session.kill',{'session_id':sid,'wait_ms':30000})
        except Exception:pass
    if target_handle:
        if kernel.WaitForSingleObject(target_handle,0)==258:kernel.TerminateProcess(target_handle,0);kernel.WaitForSingleObject(target_handle,5000)
        kernel.CloseHandle(target_handle)
    if service:service.terminate();service.wait(8)
    save('summary',{'passed':checks});print('Evidence:',run,flush=True)
