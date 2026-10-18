// Stops only its own temporary server. Run while the desktop app is closed.
const {spawn}=require('node:child_process'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const root=path.resolve(process.argv[2]||'.'),dir=fs.mkdtempSync(path.join(require('node:os').tmpdir(),'laoren-features-'));let child;
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function api(endpoint,body){const r=await fetch('http://127.0.0.1:8765/api/'+endpoint,body===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});assert(r.ok,endpoint+':'+r.status);return r.json()}
async function boot(){child=spawn(path.join(root,'bin/laoren-server'),[],{cwd:root,env:{...process.env,LAOREN_DATA_DIR:dir},stdio:'ignore'});for(let i=0;i<60;i++){await pause(100);if(child.exitCode!==null)throw Error('stop desktop app before test');try{return await api('state')}catch{}}throw Error('server start failed')}
async function stop(){if(!child)return;const c=child;child=null;await new Promise(r=>{c.once('exit',r);c.kill('SIGTERM')})}
(async()=>{try{
await boot();await api('config',{push_channels:[],source_mode:'demo',person_confidence:0.55,multiple_confidence:0.8});await api('start',{mode:'demo'});await api('demo',{pose:'lying',select:true});await pause(2300);assert((await api('state')).armed);await api('demo',{pose:'multiple',select:true});assert.equal((await api('state')).pose,'multiple');assert(!(await api('state')).armed);await api('demo',{pose:'upright',select:true});await pause(700);assert(!(await api('state')).armed);await api('stop',{});
// Calibration cannot be fabricated through the public config endpoint.
await api('config',{assist_lying:{head:[0.4,0.7]},assist_upright:{head:[0.4,0.3]},lost_after:45});
assert.equal((await api('state')).config.assist_lying,null);
let rejected=await fetch('http://127.0.0.1:8765/api/calibrate',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({step:'lying'})});assert.equal(rejected.status,400);
const before=await api('state');assert(before.events.length>=3);assert(before.events.every(e=>/^\d{4}-\d{2}-\d{2}$/.test(e.date)));await stop();let state=await boot();assert.equal(state.events.length,before.events.length);assert(Math.abs(state.config.person_confidence-0.55)<1e-6);assert(Math.abs(state.config.multiple_confidence-0.8)<1e-6);assert.equal(state.config.lost_after,45);
// Simulate a legitimately persisted prior calibration, then verify source/ROI invalidation.
await stop();let cfg=JSON.parse(fs.readFileSync(path.join(dir,'config.json'),'utf8'));Object.assign(cfg,{assist_enabled:true,assist_lying:{head:[0.4,0.7]},assist_upright:{head:[0.4,0.3]}});fs.writeFileSync(path.join(dir,'config.json'),JSON.stringify(cfg));state=await boot();assert(state.config.assist_enabled&&state.calibration.ready);
await api('config',{assist_enabled:false});
let override=await fetch('http://127.0.0.1:8765/api/start',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({mode:'camera',source:'http://127.0.0.1:9/alternate'})});assert.equal(override.status,400,'source override must not retain old calibration');
await api('config',{bed:[0.1,0.1,0.8,0.8]});state=await api('state');assert(!state.config.assist_enabled&&!state.calibration.ready);assert.equal(state.config.assist_lying,null);
const source='rtsp://dummy:dummy@127.0.0.1:9/camera';await api('config',{source_mode:'camera',source});assert.equal((await api('source/reveal',{mode:'camera'})).value,source);assert(!JSON.stringify(await api('state')).includes('dummy@'));await api('config',{clear_source:true});assert.equal((await api('source/reveal',{mode:'camera'})).value,'');
await api('config',{source:'http://127.0.0.1:9/invalid-video',autostart:true});await stop();state=await boot();assert(state.running);assert.equal(state.mode,'camera');assert(state.events.some(e=>e.text.includes('自动恢复摄像头')));await api('stop',{});await api('config',{autostart:false});
if(fs.existsSync(path.join(root,'work/blank-test.avi'))){await api('config',{source_mode:'replay',schedule_mode:'all_day',source:path.join(root,'work/blank-test.avi')});await api('start',{mode:'replay'});let done;for(let i=0;i<80;i++){await pause(200);done=await api('state');if(!done.running)break;}assert(done.video_duration>0);assert(done.video_position>0);assert.equal(done.message,'视频分析完成');}
console.log('PASS: demo lying/multiple/reset, persistent dated history, calibration validation/invalidation, lost visibility setting, confidence setting, private source reveal/clear, auto-start, replay duration/progress/completion');
}finally{await stop();fs.rmSync(dir,{recursive:true,force:true})}})().catch(e=>{console.error(e);process.exitCode=1});
