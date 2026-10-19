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
const elements=Object.fromEntries(['captureStatus','alertCaptures','captureTitle','captureImage','captureDialog','closeCapture','captureFrames','captureFrameInfo','capturePrev','captureNext','captureMissing'].map(id=>[id,new Element('div')]));
const $=id=>elements[id],el=(tag,text)=>new Element(tag,text),document={createElement:tag=>new Element(tag)};
const current={alert_captures:[],capture_status:'本机保留五次'};
const all=fs.readFileSync('web/app.js','utf8');const source=all.slice(all.indexOf("let captureKey="));
const render=new Function('$','el','document','current',source+';return renderCaptures;')($,el,document,current);
render();assert(elements.alertCaptures.children[0].textContent.includes('暂无告警画面'));
current.alert_captures=[{id:'one/two',time:'2026-10-18 22:00:00',label:'已经坐起',has_image:true,demo:false},{id:'missing',time:'time',label:'断流',has_image:false,demo:false}];
render();assert.equal(elements.alertCaptures.children.length,2);
const button=elements.alertCaptures.children[0].children[0];assert.equal(button.children[0].src,'/api/alert-capture/one%2Ftwo');
button.onclick();assert.equal(elements.captureDialog.open,true);assert.equal(elements.captureImage.src,button.children[0].src);assert(elements.captureTitle.textContent.includes('已经坐起'));
elements.closeCapture.onclick();assert.equal(elements.captureDialog.open,false);assert.equal(elements.captureImage.src,undefined);
assert(elements.alertCaptures.children[1].children[0].textContent.includes('无新鲜画面'));
render();assert.equal(elements.alertCaptures.children.length,2);
console.log('PASS: alert gallery empty state, image viewer, close, missing image and refresh');

current.alert_captures.push({id:'group',time:'now',label:'已经坐起',has_image:true,demo:false,frames:[{time:'22:00:00.000',pose:'upright',has_image:true},{time:'22:00:00.500',pose:'unknown',has_image:false},{time:'22:00:01.000',pose:'upright',has_image:true}]});
render();elements.alertCaptures.children[2].children[0].onclick();
assert.equal(elements.captureImage.src,'/api/alert-capture/group/frame/0');assert.equal(elements.capturePrev.disabled,true);assert.equal(elements.captureNext.disabled,false);
elements.captureNext.onclick();assert.equal(elements.captureImage.hidden,true);assert.equal(elements.captureMissing.hidden,false);assert(elements.captureFrameInfo.textContent.includes('无法判断'));
elements.captureNext.onclick();assert.equal(elements.captureImage.src,'/api/alert-capture/group/frame/2');assert.equal(elements.captureImage.hidden,false);assert.equal(elements.captureNext.disabled,true);
elements.captureFrames.children[0].onclick();assert.equal(elements.captureImage.src,'/api/alert-capture/group/frame/0');
elements.captureDialog.onclose();assert.equal(elements.captureImage.src,undefined);assert.equal(elements.captureFrames.children.length,0);
console.log('PASS: grouped frame navigation, unknown/missing frames, first/last bounds, jump and close');
