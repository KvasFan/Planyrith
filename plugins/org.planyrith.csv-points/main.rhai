fn build(p) {
    let created=[];
    for row in p.csv.split("\n") {
        row.trim();
        if row == "" {continue;}
        let fields=row.split(",");
        if fields.len()!=3 {throw "Each CSV row must have x,y,z";}
        let point=pt(parse_float(fields[0])*p.scale,parse_float(fields[1])*p.scale,parse_float(fields[2])*p.scale);
        let r=tape_measure(#{from:pt(0,0,0),to:point,guide:"point",create_guide:true});
        if r.guide != () {created.push("@" + r.guide);}
    }
    if created.len()==0 {throw "CSV contains no points";}
    make_group(#{targets:created.join(" | "),name:"CSV points"})
}
