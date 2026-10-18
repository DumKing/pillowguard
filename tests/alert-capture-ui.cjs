const fs=require('node:fs'),assert=require('node:assert/strict');
class Element {
  constructor(tag,text){this.tag=tag;this.textContent=text;this.children=[];this.attributes={};}
  append(...children){this.children.push(...children)}
  replaceChildren(){this.children=[]}
  setAttribute(key,value){this.attributes[key]=value}
  removeAttribute(key){delete this[key]}
  showModal(){this.open=true}
  close(){this.open=false}
}
const elements=Object.fromEntries(['captureStatus','alertCaptures','captureTitle','captureImage','captureDialog','closeCapture'].map(id=>[id,new Element('div')]));
const $=id=>elements[id],el=(tag,text)=>new Element(tag,text),document={createElement:tag=>new Element(tag)};
const current={alert_captures:[],capture_status:'本机保留五次'};
const source=fs.readFileSync('web/app.js','utf8').split("let captureKey='';")[1];
const render=new Function('$','el','document','current',"let captureKey='';"+source+';return renderCaptures;')($,el,document,current);
render();assert(elements.alertCaptures.children[0].textContent.includes('暂无告警画面'));
current.alert_captures=[{id:'one/two',time:'2026-10-18 22:00:00',label:'已经坐起',has_image:true,demo:false},{id:'missing',time:'time',label:'断流',has_image:false,demo:false}];
render();assert.equal(elements.alertCaptures.children.length,2);
const button=elements.alertCaptures.children[0].children[0];assert.equal(button.children[0].src,'/api/alert-capture/one%2Ftwo');
button.onclick();assert.equal(elements.captureDialog.open,true);assert.equal(elements.captureImage.src,button.children[0].src);assert(elements.captureTitle.textContent.includes('已经坐起'));
elements.closeCapture.onclick();assert.equal(elements.captureDialog.open,false);assert.equal(elements.captureImage.src,undefined);
assert(elements.alertCaptures.children[1].children[0].textContent.includes('无新鲜画面'));
render();assert.equal(elements.alertCaptures.children.length,2);
console.log('PASS: alert gallery empty state, image viewer, close, missing image and refresh');
