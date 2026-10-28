fn board(a,b,h,name) {
    rectangle(#{corner_a:a,corner_b:b});
    push_pull(#{face:sel("face & last"),distance:h});
    let r = make_group(#{targets:"edge | face",name:name});
    "@" + r.group
}
fn build(p) {
    let t=p.thickness;
    if 2*t>=p.width || 2*t>=p.height || t>=p.depth {throw "Thickness exceeds the cabinet dimensions";}
    let parts=[];
    parts.push(board(pt(0,0,0),pt(p.width,p.depth,0),t,"Bottom"));
    parts.push(board(pt(0,0,p.height-t),pt(p.width,p.depth,p.height-t),t,"Top"));
    parts.push(board(pt(0,0,t),pt(t,p.depth,t),p.height-2*t,"Left side"));
    parts.push(board(pt(p.width-t,0,t),pt(p.width,p.depth,t),p.height-2*t,"Right side"));
    parts.push(board(pt(t,p.depth-t,t),pt(p.width-t,p.depth,t),p.height-2*t,"Back"));
    parts.push(board(pt(t,0,p.height/2),pt(p.width-t,p.depth-t,p.height/2),t,"Shelf"));
    make_group(#{targets:parts.join(" | "),name:"Cabinet"})
}
