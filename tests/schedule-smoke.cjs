const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),assert=require('node:assert/strict'),{spawn}=require('node:child_process');
const root=path.resolve('.'),dir=fs.mkdtempSync(path.join(os.tmpdir(),'pillowguard-schedule-'));let child;const wait=ms=>new Promise(r=>setTimeout(r,ms));
async function api(name,data){const r=await fetch('http://127.0.0.1:8765/api/'+name,data===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});if(!r.ok)throw Error(name+':'+r.status);return r.json()}
async function boot(){child=spawn(path.join(root,'bin/pillowguard-server'),[],{cwd:root,env:{...process.env,PILLOWGUARD_DATA_DIR:dir},stdio:'ignore'});for(let n=0;n<60;n++){await wait(100);if(child.exitCode!==null)throw Error('close desktop app first');try{return await api('state')}catch{}}throw Error('server did not start')}
async function stop(){if(!child)return;const c=child;child=null;await new Promise(r=>{c.once('exit',r);c.kill('SIGTERM')})}
function active(w,t){return w.some(v=>v.start<v.end?v.start<=t&&t<v.end:t>=v.start||t<v.end)}
(async()=>{try{
fs.writeFileSync(path.join(dir,'config.json'),JSON.stringify({schedule_mode:'custom',schedule_start:'23:00',schedule_end:'06:00'}));
let s=await boot();assert.equal(s.config.schedule_windows,null);assert.equal(s.schedule.active,active([{start:'23:00',end:'06:00'}],s.schedule.clock));
const windows=[{start:'13:00',end:'14:00'},{start:'22:00',end:'07:00'},{start:'06:30',end:'08:00'}];await api('config',{schedule_windows:windows});s=await api('state');assert.deepEqual(s.config.schedule_windows,windows);assert.equal(s.schedule.active,active(windows,s.schedule.clock));assert.equal(s.config.schedule_start,'13:00');
await stop();s=await boot();assert.deepEqual(s.config.schedule_windows,windows);assert.equal(s.schedule.active,active(windows,s.schedule.clock));
for(const bad of [[],[{start:'13:00',end:'13:00'}],[{start:'25:00',end:'07:00'}],Array(17).fill(windows[0])]){const r=await fetch('http://127.0.0.1:8765/api/config',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({schedule_windows:bad})});assert.equal(r.status,400);assert.deepEqual((await api('state')).config.schedule_windows,windows)}
await api('config',{schedule_mode:'all_day'});assert((await api('state')).schedule.active);assert.deepEqual((await api('state')).config.schedule_windows,windows);
await api('config',{schedule_mode:'custom',schedule_start:'21:00',schedule_end:'05:00'});s=await api('state');assert.equal(s.config.schedule_windows,null,'legacy import must replace the saved interval list');assert.equal(s.config.schedule_start,'21:00');assert.equal(s.schedule.active,active([{start:'21:00',end:'05:00'}],s.schedule.clock));
console.log('PASS: multiple-window persistence, union scheduling, midnight compatibility, limits and invalid rejection, all-day switching, legacy single-window import');
}finally{await stop();fs.rmSync(dir,{recursive:true,force:true})}})().catch(e=>{console.error(e);process.exitCode=1});
