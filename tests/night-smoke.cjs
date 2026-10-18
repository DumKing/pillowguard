// Synthetic black frames only: no real camera, credentials or external recipients.
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),assert=require('node:assert/strict'),{spawn}=require('node:child_process');
const root=path.resolve(process.argv[2]||'.'),dir=fs.mkdtempSync(path.join(os.tmpdir(),'zhenan-night-'));let child,receiver;const messages=[];
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function api(name,data){const r=await fetch('http://127.0.0.1:8765/api/'+name,data===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});assert(r.ok,name+':'+r.status);return r.json()}
(async()=>{try{
receiver=http.createServer((req,res)=>{let body='';req.on('data',b=>body+=b);req.on('end',()=>{messages.push(JSON.parse(body));res.end('{}')})});await new Promise(r=>receiver.listen(0,'127.0.0.1',r));
const fake=path.join(dir,'frames.cjs');fs.writeFileSync(fake,'#!/usr/bin/env node\nconst frame=Buffer.alloc(1280*720*3);setInterval(()=>{process.stdout.write(frame)},250);\n',{mode:0o700});
child=spawn(path.join(root,'bin/zhenan-server'),[],{cwd:root,env:{...process.env,ZHENAN_DATA_DIR:dir,ZHENAN_FFMPEG:fake},stdio:'ignore'});
for(let n=0;n<60;n++){await pause(100);if(child.exitCode!==null)throw Error('stop the existing app before testing');try{await api('state');break}catch{}}
await api('config',{source_mode:'camera',source:'http://127.0.0.1:9/synthetic',lost_after:10,lost_alert_enabled:false,push_channels:[{id:'local',name:'本地模拟接收端',kind:'webhook',enabled:true,url:'http://127.0.0.1:'+receiver.address().port}]});await api('start',{mode:'camera'});
await pause(12000);assert.equal(messages.length,0,'disabled anomaly reminders must stay silent');
await api('config',{lost_alert_enabled:true});
for(let n=0;n<100&&!messages.length;n++)await pause(200);
assert.equal(messages.length,1,'prolonged inability to judge must notify automatically');const body=JSON.stringify(messages[0]);assert(body.includes('监测异常'));assert(body.includes('无法判断'));assert(!body.includes('检测到老人坐起'));
let state=await api('state');assert(state.last_frame&&state.running);assert.equal(state.people,0);assert(state.events.some(e=>e.text.includes('持续无法判断')));
await pause(1500);assert.equal(messages.length,1,'no notification flooding');await api('config',{lost_alert_enabled:false});await api('stop',{});await api('start',{mode:'demo'});await pause(11000);assert.equal(messages.length,1,'demo must not send visibility warnings');await api('stop',{});
console.log('PASS: real inference on synthetic blank frames, timed automatic visibility warning via local webhook, separate wording, no flooding, no demo warning');
}finally{if(child){const c=child;await new Promise(r=>{c.once('exit',r);c.kill('SIGTERM')})}if(receiver)await new Promise(r=>receiver.close(r));fs.rmSync(dir,{recursive:true,force:true})}})().catch(e=>{console.error(e);process.exitCode=1});
