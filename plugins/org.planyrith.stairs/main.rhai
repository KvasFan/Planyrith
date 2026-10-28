fn build(p) {
    let created = [];
    for i in 0..p.steps {
        let y = p.run * i / p.steps;
        rectangle(#{corner_a: pt(0,y,0), corner_b: pt(p.width,p.run*(i+1)/p.steps,0)});
        push_pull(#{face: sel("face & last"), distance: p.rise*(i+1)/p.steps});
        let tread = make_group(#{targets: "edge | face", name: "Tread " + (i+1)});
        created.push("@" + tread.group);
    }
    make_group(#{targets: created.join(" | "), name: "Stairs"})
}
