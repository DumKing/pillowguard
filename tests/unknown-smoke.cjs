// Temporary dummy data and a local receiver only. Desktop app must be closed.
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),assert=require('node:assert/strict'),{spawn}=require('node:child_process');
const root=path.resolve(process.argv[2]||'.'),dir=fs.mkdtempSync(path.join(os.tmpdir(),'zhenan-unknown-'));let child,receiver;const messages=[];
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function api(name,data){const r=await fetch('http://127.0.0.1:8765/api/'+name,data===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});assert(r.ok,name+':'+r.status);return r.json()}
async function boot(){child=spawn(path.join(root,'bin/zhenan-server'),[],{cwd:root,env:{...process.env,ZHENAN_DATA_DIR:dir},stdio:'ignore'});for(let n=0;n<60;n++){await pause(100);if(child.exitCode!==null)throw Error('stop the desktop app before testing');try{return await api('state')}catch{}}throw Error('server not ready')}
async function stop(){if(!child)return;const c=child;child=null;await new Promise(r=>{c.once('exit',r);c.kill('SIGTERM')})}
async function transition(){await api('demo',{pose:'unknown',select:true});await pause(2400);await api('demo',{pose:'upright',select:true});await pause(1200)}
(async()=>{try{
receiver=http.createServer((req,res)=>{let b='';req.on('data',v=>b+=v);req.on('end',()=>{messages.push(JSON.parse(b));res.end('{}')})});await new Promise(r=>receiver.listen(0,'127.0.0.1',r));
let state=await boot();assert.equal(state.config.alert_from_unknown,false);assert.equal(state.config.care.confirmation,false,'manual confirmation is optional and off by default');
await api('config',{lost_alert_enabled:false,hold:0.5,cooldown:5,push_channels:[{id:'local',name:'本地测试',kind:'webhook',enabled:true,url:'http://127.0.0.1:'+receiver.address().port}]});
await api('start',{mode:'demo'});await transition();assert.equal(messages.length,0,'disabled policy must require lying');await api('stop',{});
await api('config',{alert_from_unknown:true});await stop();state=await boot();assert.equal(state.config.alert_from_unknown,true,'option survives restart');assert.equal(state.config.lost_alert_enabled,false,'anomaly reminder switch survives restart without suppressing rise alerts');assert.equal(state.calibration.ready,false,'no calibration required');
await api('start',{mode:'demo'});await transition();assert.equal(messages.length,1);assert(JSON.stringify(messages[0]).includes('无法识别后检测到坐起'));assert(JSON.stringify(messages[0]).includes('演示告警'));assert(!JSON.stringify(messages[0]).includes('[告警 '),'automatic mode must not ask for console acknowledgement');
await pause(5500);assert.equal(messages.length,1,'remaining upright must not repeat');
await api('demo',{pose:'multiple',select:true});await pause(300);await api('demo',{pose:'upright',select:true});await pause(1200);assert.equal(messages.length,1,'multiple must clear transition readiness');
await transition();assert.equal(messages.length,2,'fresh stable unknown can rearm');
await api('stop',{});console.log('PASS: optional unknown-to-upright alert, off by default, persistence, no calibration dependency, local notification wording, no repeated sitting alert, multi pause and rearming');
}finally{await stop();if(receiver)await new Promise(r=>receiver.close(r));fs.rmSync(dir,{recursive:true,force:true})}})().catch(e=>{console.error(e);process.exitCode=1});
