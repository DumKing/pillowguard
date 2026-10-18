// Temporary local data only: compatibility, polygon persistence, invalid shapes,
// calibration invalidation and inactive auxiliary regions.
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),assert=require('node:assert/strict'),{spawn}=require('node:child_process');
const root=path.resolve('.'),dir=fs.mkdtempSync(path.join(os.tmpdir(),'zhenan-regions-'));let child;const wait=ms=>new Promise(r=>setTimeout(r,ms));
async function api(name,data){const r=await fetch('http://127.0.0.1:8765/api/'+name,data===undefined?{}:{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});if(!r.ok)throw Error(name+':'+r.status);return r.json()}
async function boot(){child=spawn(path.join(root,'bin/zhenan-server'),[],{cwd:root,env:{...process.env,ZHENAN_DATA_DIR:dir},stdio:'ignore'});for(let n=0;n<60;n++){await wait(100);if(child.exitCode!==null)throw Error('stop the desktop app first');try{return await api('state')}catch{}}throw Error('server did not start')}
async function stop(){if(!child)return;const c=child;child=null;await new Promise(r=>{c.once('exit',r);c.kill('SIGTERM')})}
function close(a,b){assert.equal(a.length,b.length);for(let k=0;k<a.length;k++)for(let j=0;j<2;j++)assert(Math.abs(a[k][j]-b[k][j])<1e-6)}
(async()=>{try{
fs.writeFileSync(path.join(dir,'config.json'),JSON.stringify({bed:[.1,.2,.8,.7],assist_enabled:true,assist_lying:{head:[.4,.7]},assist_upright:{head:[.4,.3]}}));
let state=await boot();assert.equal(state.config.bed.length,4);await api('config',{bed:[[.1,.2],[.9,.2],[.9,.9],[.1,.9]],push_channels:[]});state=await api('state');assert(state.calibration.ready,'rectangle-to-polygon conversion preserves equivalent calibration');
const bed=[[.4,.1],[.6,.1],[.9,.9],[.1,.9]],edge=[[.1,.7],[.9,.7],[.9,.8],[.1,.8]],floor=[[.02,.9],[.3,.85],[.4,.95],[.02,.98]];
await api('config',{bed,care:{...state.config.care,edge_region:edge,floor_region:floor}});state=await api('state');assert(!state.calibration.ready&&!state.config.assist_enabled);close(state.config.bed,bed);close(state.config.care.edge_region,edge);close(state.config.care.floor_region,floor);assert(!state.config.care.edge&&!state.config.care.fall);
for(const bad of [[],[[0,0],[1,1],[0,1],[1,0]],[[0,0],[1.2,0],[1,1]],[[.1,.1],[.11,.1],[.1,.11]]]){const r=await fetch('http://127.0.0.1:8765/api/config',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({bed:bad})});assert.equal(r.status,400);close((await api('state')).config.bed,bed)}
await stop();state=await boot();close(state.config.bed,bed);close(state.config.care.edge_region,edge);close(state.config.care.floor_region,floor);assert(!state.config.care.edge&&!state.config.care.fall);
await api('config',{care:{...state.config.care,edge_region:null}});assert.equal((await api('state')).config.care.edge_region,null);
console.log('PASS: legacy rectangle compatibility, equivalent-geometry calibration, polygon persistence for three regions, self-crossing/outside/tiny rejection, real geometry calibration reset, auxiliary alerts stay disabled');
}finally{await stop();fs.rmSync(dir,{recursive:true,force:true})}})().catch(e=>{console.error(e);process.exitCode=1});
