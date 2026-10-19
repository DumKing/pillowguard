const fs=require('node:fs'),assert=require('node:assert/strict');
const js=fs.readFileSync('web/app.js','utf8');
function el(tag,text,cls){return {tag,text,cls,children:[],attrs:{},append(...nodes){this.children.push(...nodes)},replaceChildren(...nodes){this.children=nodes},setAttribute(k,v){this.attrs[k]=v}}}
let edits=0;
const render=new Function('el','markDirty',js.split('\n').find(l=>l.startsWith('function speakerPlayback('))+';return speakerPlayback')(el,()=>edits++);
const card=el('section'),draft={name:'音箱'};render(card,draft);
assert.deepEqual(draft.speaker_playback,{default_count:1,windows:[]});
const [defaultLabel,rows,add]=card.children;
defaultLabel.children[0].value='2';defaultLabel.children[0].oninput();assert.equal(draft.speaker_playback.default_count,2);
add.onclick();add.onclick();assert.equal(rows.children.length,2);
const start=rows.children[1].children[0].children[0];start.value='23:00';start.oninput();assert.equal(draft.speaker_playback.windows[1].start,'23:00');
rows.children[0].children[3].onclick();assert.equal(draft.speaker_playback.windows.length,1);assert.equal(draft.speaker_playback.windows[0].start,'23:00');
for(let i=0;i<20;i++)add.onclick();assert.equal(draft.speaker_playback.windows.length,16);assert.equal(add.disabled,true);
const other={name:'另一音箱'},otherCard=el('section');render(otherCard,other);assert.equal(other.speaker_playback.default_count,1);assert.equal(other.speaker_playback.windows.length,0);
assert(edits>0);console.log('PASS: per-speaker default, multiple windows, editing, removal and 16-window limit');

(async()=>{
 let calls=[],fail=false;
 const test=new Function('save','post','toast','poll',js.split('\n').find(l=>l.startsWith('async function testSpeakerPlayback('))+';return testSpeakerPlayback')(async()=>{if(fail)throw Error('save failed')},async(path,body)=>calls.push({path,body}),()=>{},async()=>{});
 await test({id:'one-speaker',enabled:true});assert.deepEqual(calls,[{path:'test-channel',body:{id:'one-speaker',use_playback_count:true}}]);
 await assert.rejects(test({id:'disabled',enabled:false}));assert.equal(calls.length,1);
 fail=true;await assert.rejects(test({id:'one-speaker',enabled:true}));assert.equal(calls.length,1);
 console.log('PASS: count test targets only the selected speaker after save; disabled or failed save never sends');
})().catch(e=>{console.error(e);process.exitCode=1});
