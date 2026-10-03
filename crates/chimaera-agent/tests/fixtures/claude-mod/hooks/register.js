let count = 0;
let value = 'Initial';
let selected = 'one';
export function register(on) {
 on('session.start', async ($, e, next) => {
   await $.command.register({name:'chimaera-mod-test',description:'Open test pane',immediate:true});
   return next(e);
 });
 on('session.attach', async ($, e, next) => { await $.ui.open({id:'chimaera-test',title:'Mod controls',closeOnEscape:true}); return next(e); });
 on('command.run', {command:'chimaera-mod-test'}, async ($) => {await $.ui.open({id:'chimaera-test',title:'Mod controls',closeOnEscape:true}); return {};});
 on('ui.render', {component:'Pane'}, async ($, e, next) => {
   if(e.requestId !== 'chimaera-test') return next(e);
   const {Box,Text,Button,Input,Select,Markdown,Code,Client}= $.ui.resolve(e);
   const redraw=()=>$.ui.invalidate('ui.render');
   return Box({flexDirection:'column',gap:1,children:[
     Text({bold:true,children:['Live Claude Mod']}),
     Text({children:['Count: '+count+' / '+value+' / '+selected]}),
     Button({key:'increment',label:'Add one',onPress:()=>{count++;redraw();}}),
     Input({key:'name',label:'Name',value,onInput:(s)=>{value=s;redraw();},onSubmit:(s)=>{value=s;redraw();}}),
     Select({key:'choice',label:'Choice',value:selected,options:[{value:'one',label:'One'},{value:'two',label:'Two'}],onSelect:(s)=>{selected=s;redraw();}}),
     Markdown({text:'**Markdown** with a [link](https://example.com)'}),
     Code({source:'const count = 1;',language:'javascript'}),
     Client({key:'client-counter',module:'./counter.tsx',props:{label:'Local counter'}}),
     Box({flexDirection:'row',gap:1,children:[
       Button({key:'fill',label:'Fill composer',onPress:async()=>{await $.prompt.fill({text:'Mod draft',mode:'replace',decorations:[{start:0,end:3,bold:true,color:'green'}]});}}),
       Button({key:'append',label:'Append note',onPress:async()=>{await $.prompt.fill({text:' note',mode:'append',decorations:[{start:1,end:5,italic:true}]});}}),
       Button({key:'suggest',label:'Suggest draft',onPress:async()=>{await $.prompt.suggest({text:'Suggested by the Mod'});}}),
       Button({key:'copy',label:'Copy text',onPress:async()=>{await $.ui.copy({text:'Mod copied text',surface:'desktop'});}})
     ]})
   ]});
 });
 on('ui.render', {component:'AbovePrompt'}, async ($, e, next) => {
   const {Text}= $.ui.resolve(e); return Text({children:['Fixture ready · '+count]});
 });
}
