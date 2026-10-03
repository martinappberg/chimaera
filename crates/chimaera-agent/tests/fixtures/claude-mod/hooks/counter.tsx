export default function Counter(props, surface) {
 const {Box,Text,Button} = surface.elements;
 const state = surface.state ?? {count:0,key:'none',pointer:'none'};
 const patch = (value) => surface.setState({...surface.state ?? state,...value});
 surface.onKey((event)=>patch({key:event.key}));
 surface.onPointer((event)=>{
   if(event.type === 'down' || event.type === 'up') patch({pointer:event.type+' '+event.x+','+event.y});
 });
 return Box({flexDirection:'column',children:[
  Text({children:[props.label + ': ' + state.count]}),
  Text({children:['Key: '+state.key+' · Pointer: '+state.pointer]}),
  Text({children:['Viewport: '+surface.columns+' × '+surface.rows]}),
  Box({flexDirection:'row',gap:1,children:[
   Button({key:'local-add',label:'Add locally',onPress:()=>patch({count:(surface.state?.count ?? 0)+1})}),
   Button({key:'post',label:'Post count',onPress:()=>surface.post({count:state.count})})
  ]})
 ]});
}
