use anyhow::Result;
use std::path::PathBuf;

const COOKED: &str = r"S:\Games\SteamLibrary\steamapps\common\Dishonored\DishonoredGame\CookedPCConsole";

fn pkg_path(name: &str) -> PathBuf {
    let p = PathBuf::from(name);
    if p.exists() { p } else { PathBuf::from(COOKED).join(format!("{name}.upk")) }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("list") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            println!("{} v{}/{} names={} imports={} exports={}", pkg.name, pkg.version, pkg.licensee, pkg.names.len(), pkg.imports.len(), pkg.exports.len());
            let filter = args.get(3).cloned();
            for i in 1..=pkg.exports.len() as i32 {
                let e = &pkg.exports[i as usize - 1];
                let cls = pkg.class_name(i);
                if let Some(f) = &filter { if !cls.eq_ignore_ascii_case(f) { continue; } }
                println!("{:6} {:30} {:9} {:8} fl={:016x} ef={:x} {}", i, cls, e.offset, e.size, e.flags, e.export_flags, pkg.obj_path(i));
            }
        }
        Some("obj") => {
            // dhtool obj <object path> [package to load first]: an object found among the
            // packages, its properties and its archetypes'
            init_classes();
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            assets.load_globals();
            if let Some(p) = args.get(3).and_then(|n| assets.package(n)) {
                assets.add_search(p);
            }
            let Some(o) = assets.find(&args[2]) else { anyhow::bail!("not found") };
            let (pkg, mut idx) = (o.pkg.clone(), o.idx);
            println!("in {}", pkg.name);
            while idx > 0 {
                println!("== {} [{}]", pkg.obj_path(idx), pkg.class_name(idx));
                if let Ok(od) = upk::read_object(&pkg, idx) { dump_props(&pkg, &od.props, 1); }
                idx = pkg.exports[idx as usize - 1].archetype;
            }
        }
        Some("chain") => {
            // dhtool chain <pkg> <export>: an object's properties, then its archetypes' in turn
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let mut idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            while idx > 0 {
                println!("== {} [{}]", pkg.obj_path(idx), pkg.class_name(idx));
                if let Ok(od) = upk::read_object(&pkg, idx) { dump_props(&pkg, &od.props, 1); }
                idx = pkg.exports[idx as usize - 1].archetype;
            }
            if idx < 0 { println!("== (import) {}", pkg.obj_path(idx)); }
        }
        Some("supers") => {
            // classes (by name filter) and their super classes
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let f = args.get(3).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            for i in 1..=pkg.exports.len() as i32 {
                let e = &pkg.exports[i as usize - 1];
                if pkg.class_name(i) != "Class" || !pkg.obj_name(i).to_ascii_lowercase().contains(&f) { continue; }
                println!("{} : {}", pkg.obj_name(i), if e.super_ != 0 { pkg.obj_path(e.super_) } else { String::new() });
            }
        }
        Some("imports") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for (i, imp) in pkg.imports.iter().enumerate() {
                println!("{:6} {}.{} {}", -(i as i32) - 1, imp.class_package, imp.class_name, pkg.obj_path(-(i as i32) - 1));
            }
        }
        Some("classes") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let mut m = std::collections::BTreeMap::new();
            for i in 1..=pkg.exports.len() as i32 { *m.entry(pkg.class_name(i)).or_insert(0) += 1; }
            for (k, v) in m { println!("{v:6} {k}"); }
        }
        Some("hex") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = args[3].parse()?;
            let d = pkg.export_data(idx)?;
            let n: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(256);
            for (row, chunk) in d[..n.min(d.len())].chunks(16).enumerate() {
                print!("{:06x}: ", row * 16);
                for b in chunk { print!("{b:02x} "); }
                println!();
            }
        }
        Some("props") => {
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            println!("{} [{}] size={}", pkg.obj_path(idx), pkg.class_name(idx), pkg.exports[idx as usize - 1].size);
            let od = upk::read_object(&pkg, idx)?;
            dump_props(&pkg, &od.props, 1);
            let e = &pkg.exports[idx as usize - 1];
            println!("native data: {} bytes remaining", e.offset + e.size - od.reader.pos);
        }
        Some("meshes") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let (mut ok, mut bad) = (0, 0);
            for i in pkg.exports_of_class("StaticMesh").collect::<Vec<_>>() {
                match upk::mesh::read_static_mesh(&pkg, i) {
                    Ok(m) => { ok += 1; if args.len() > 3 { println!("OK {} ext={:?} v={} i={} uv={} sec={} mats={:?}", pkg.obj_path(i), m.bounds_extent, m.positions.len(), m.indices.len(), m.uvs.len(), m.sections.len(), m.sections.iter().map(|s| pkg.obj_path(s.material)).collect::<Vec<_>>()); } }
                    Err(e) => { bad += 1; println!("FAIL {}: {:#}", pkg.obj_path(i), e); }
                }
            }
            println!("ok={ok} fail={bad}");
        }
        Some("mics") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let mut names = std::collections::BTreeMap::new();
            for i in pkg.exports_of_class("MaterialInstanceConstant").collect::<Vec<_>>() {
                let od = upk::read_object(&pkg, i)?;
                if let Some((c, o, s)) = od.props.array("TextureParameterValues") {
                    for e in upk::props::parse_struct_array(&pkg, o, s, c)? {
                        let n = e.name("ParameterName").unwrap_or("?").to_string();
                        let t = e.object("ParameterValue").map(|t| pkg.obj_path(t)).unwrap_or_default();
                        if args.len() > 3 { println!("{} {} = {}", pkg.obj_path(i), n, t); }
                        *names.entry(n).or_insert(0) += 1;
                    }
                }
            }
            for (k, v) in names { println!("{v:5} {k}"); }
        }
        Some("textures") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let tfc = upk::texture::TfcCache::new(std::path::Path::new(COOKED));
            let (mut ok, mut bad) = (0, 0);
            let mut fmts = std::collections::BTreeMap::new();
            for i in pkg.exports_of_class("Texture2D").collect::<Vec<_>>() {
                match upk::texture::read_texture2d(&pkg, i, &tfc, 4096) {
                    Ok(t) => { ok += 1; *fmts.entry(t.format_name.clone()).or_insert(0) += 1; if args.len() > 3 { println!("OK {} {} {}x{} mips={}", pkg.obj_path(i), t.format_name, t.mips[0].width, t.mips[0].height, t.mips.len()); } }
                    Err(e) => { bad += 1; println!("FAIL {}: {:#}", pkg.obj_path(i), e); }
                }
            }
            println!("ok={ok} fail={bad} {:?}", fmts);
        }
        Some("texpng") => {
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let tfc = upk::texture::TfcCache::new(std::path::Path::new(COOKED));
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            let t = upk::texture::read_texture2d(&pkg, idx, &tfc, 4096)?;
            let m = &t.mips[0];
            let rgba = decode_rgba(t.format, m.width, m.height, &m.data);
            image::save_buffer(&args[4], &rgba, m.width, m.height, image::ColorType::Rgba8)?;
            println!("wrote {} {}x{} {}", args[4], m.width, m.height, t.format_name);
        }
        Some("cook-all") => {
            init_classes();
            let mut opts = dhcook::CookOptions::default();
            if let Some(i) = args.iter().position(|a| a == "--max") { opts.max_texture_size = args[i + 1].parse()?; }
            let root = std::path::PathBuf::from("cache");
            let maps = dhcook::list_maps(std::path::Path::new(COOKED));
            let total = std::time::Instant::now();
            let mut failed = Vec::new();
            for m in &maps {
                // fresh asset set per map keeps memory bounded
                let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
                let t = std::time::Instant::now();
                match dhcook::cook_map(&assets, m, &root, opts.clone(), &|_, _| {}) {
                    Ok(_) => println!("{m:40} ok in {:.1}s", t.elapsed().as_secs_f32()),
                    Err(e) => {
                        println!("{m:40} FAILED: {e:#}");
                        failed.push(m.clone());
                    }
                }
            }
            println!("cooked {} maps in {:.1}s, {} failed {:?}", maps.len(), total.elapsed().as_secs_f32(), failed.len(), failed);
            // the movies (those not cooked yet; ffmpeg's Bink decoder)
            let game = std::path::Path::new(COOKED).parent().unwrap().to_path_buf();
            match dhcook::movies::cook_movies(&game, &root, false) {
                Ok(idx) => println!("movies: {} cooked", idx.movies.len()),
                Err(e) => println!("movies: {e:#}"),
            }
        }
        Some("cook-movies") => {
            // dhtool cook-movies [--force]: the Bink movies through ffmpeg (cache/movies)
            let game = std::path::Path::new(COOKED).parent().unwrap().to_path_buf();
            let t = std::time::Instant::now();
            let idx = dhcook::movies::cook_movies(&game, std::path::Path::new("cache"), args.iter().any(|a| a == "--force"))?;
            println!("cooked {} movies in {:.1}s ({} maps, {} subtitled)", idx.movies.len(), t.elapsed().as_secs_f32(), idx.maps.len(), idx.subtitles.len());
        }
        Some("cook") => {
            init_classes();
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            let mut opts = dhcook::CookOptions::default();
            if args.iter().any(|a| a == "--force") { opts.force = true; }
            if let Some(i) = args.iter().position(|a| a == "--max") { opts.max_texture_size = args[i + 1].parse()?; }
            let root = std::path::PathBuf::from("cache");
            let t = std::time::Instant::now();
            let last = std::sync::Mutex::new(String::new());
            let p = dhcook::cook_map(&assets, &args[2], &root, opts, &|f, msg| {
                let mut l = last.lock().unwrap();
                if *l != msg && !msg.starts_with("Textures") || msg.ends_with("0/") { }
                if *l != msg { *l = msg.to_string(); if !msg.starts_with("Textures") || f >= 0.98 { eprintln!("[{:3.0}%] {msg}", f * 100.0); } }
            })?;
            println!("wrote {} in {:.1}s", p.display(), t.elapsed().as_secs_f32());
        }
        Some("skel") => {
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for i in pkg.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
                match dhcook::skeletal::read_skeletal_mesh(&pkg, i) {
                    Ok(m) => {
                        let mut mn = [f32::MAX; 3]; let mut mx = [f32::MIN; 3];
                        for p in &m.positions { for k in 0..3 { mn[k] = mn[k].min(p[k]); mx[k] = mx[k].max(p[k]); } }
                        println!("OK {} verts={} tris={} bones={} secs={} min={:?} max={:?} origin={:?} rot={:?} radius={} lods={:?} factors={:?}", pkg.obj_path(i), m.positions.len(), m.indices.len() / 3, m.bones.len(), m.sections.len(), mn, mx, m.origin, m.rot_origin, m.radius, m.lods.iter().map(|l| (l.positions.len(), l.indices.len() / 3, l.sections.len())).collect::<Vec<_>>(), m.lod_factors);
                        if args.len() > 3 && pkg.obj_path(i).contains(&args[3]) {
                            for (bi, b) in m.bones.iter().enumerate() { println!("   {bi:3} {:30} parent={:3} pos={:?} rot={:?}", b.name, b.parent, b.position, b.rotation); }
                        }
                    }
                    Err(e) => println!("FAIL {}: {e:#}", pkg.obj_path(i)),
                }
            }
        }
        Some("animdump") => {
            // dhtool animdump <file.anim> <clip>: a cooked clip's tracks (bone, keys, value range)
            let f = dhcook::format::AnimFile::read(std::path::Path::new(&args[2]))?;
            for c in f.clips.iter().filter(|c| c.name.eq_ignore_ascii_case(&args[3])) {
                println!("{} {:.2}s {} fps {} frames", c.name, c.duration, c.rate, c.frames);
                for t in &c.rotations {
                    let ang: Vec<f32> = t.values.iter().map(|q| { let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt().max(1e-9); 2.0 * (q[3].abs() / n).min(1.0).acos().to_degrees() }).collect();
                    println!("  rot {} keys {} angle {:.2}..{:.2} first {:?}", f.bones[t.bone as usize], t.values.len(), ang.iter().cloned().fold(f32::MAX, f32::min), ang.iter().cloned().fold(f32::MIN, f32::max), t.values.first());
                }
                for t in &c.translations {
                    let mn = t.values.iter().fold([f32::MAX; 3], |a, v| [a[0].min(v[0]), a[1].min(v[1]), a[2].min(v[2])]);
                    let mx = t.values.iter().fold([f32::MIN; 3], |a, v| [a[0].max(v[0]), a[1].max(v[1]), a[2].max(v[2])]);
                    println!("  pos {} keys {} min {:?} max {:?}", f.bones[t.bone as usize], t.values.len(), mn, mx);
                }
            }
        }
        Some("animscan") => {
            // dhtool animscan <pkg>...: decode every Edge AnimSequence, report odd channels
            init_classes();
            let (mut n, mut fails, mut with_s, mut with_u, mut q48) = (0, 0, 0, 0, 0);
            for name in &args[2..] {
                let pkg = upk::Package::open(&pkg_path(name))?;
                for i in pkg.exports_of_class("AnimSequence").collect::<Vec<_>>() {
                    let Ok(so) = upk::read_object(&pkg, i) else { continue };
                    let mut r = so.reader;
                    let (Ok(_), Ok(len)) = (r.i32(), r.i32()) else { continue };
                    let Ok(blob) = r.bytes(len.max(0) as usize) else { continue };
                    if blob.len() < 0x60 { continue; }
                    n += 1;
                    let c = |k: usize| u16::from_le_bytes([blob[0x16 + 2 * k], blob[0x17 + 2 * k]]);
                    if c(2) + c(6) > 0 { with_s += 1; if with_s <= 5 { println!("scale: {} {}", name, pkg.obj_path(i)); } }
                    if c(3) + c(7) > 0 { with_u += 1; if with_u <= 5 { println!("user: {} {}", name, pkg.obj_path(i)); } }
                    if blob[0x26] & 1 == 0 { q48 += 1; }
                    if let Err(e) = dhcook::edge::EdgeAnim::decode(blob) { fails += 1; if fails <= 5 { println!("FAIL {} {}: {e:#}", name, pkg.obj_path(i)); } }
                }
            }
            println!("sequences={n} fails={fails} scale={with_s} user={with_u} q48={q48}");
        }
        Some("structs") => {
            // dhtool structs <pkg> <export> <array prop>: an array of structs, element by element
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = args[3].parse()?;
            let od = upk::read_object(&pkg, idx)?;
            if let Some((c, off, sz)) = od.props.array(&args[4]) {
                for (i, e) in upk::props::parse_struct_array(&pkg, off, sz, c)?.iter().enumerate() {
                    println!("[{i}]");
                    dump_props(&pkg, e, 1);
                }
            }
        }
        Some("names") => {
            // dhtool names <pkg>: the name table
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for (i, n) in pkg.names.iter().enumerate() {
                println!("{i} {n}");
            }
        }
        Some("dumpexport") => {
            // dhtool dumpexport <pkg> <export> <out>: raw (decompressed) export bytes
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = args[3].parse()?;
            std::fs::write(&args[4], pkg.export_data(idx)?)?;
        }
        Some("sm3all") => {
            // dhtool sm3all [filter]: translate every shader of the reference shader cache to
            // WGSL and validate it; writes failures to cache/debug/sm3_fail.txt
            let pkg = upk::Package::open(&pkg_path("RefShaderCache-PC-D3D-SM3"))?;
            let idx = pkg.exports_of_class("ShaderCache").next().expect("no ShaderCache");
            let e = &pkg.exports[idx as usize - 1];
            let data = pkg.export_data(idx)?;
            let recs = dhcook::shadercache::records(data, &pkg.names, e.offset)?;
            let filter = args.get(2).cloned().unwrap_or_default();
            let mut stats: std::collections::BTreeMap<String, (u32, u32, u32)> = Default::default();
            let mut fails = String::new();
            let mut first_err: std::collections::BTreeMap<String, u32> = Default::default();
            for r in &recs {
                if !r.ty.contains(&filter) {
                    continue;
                }
                let st = stats.entry(r.ty.clone()).or_default();
                st.0 += 1;
                let code = &data[r.code.clone()];
                let res = dhcook::sm3::parse(code).map_err(|e| format!("parse: {e}")).and_then(|sh| {
                    let (body, iface) = dhcook::sm3::translate(&sh, "sm3_").map_err(|e| format!("translate: {e}"))?;
                    let m = dhcook::sm3::standalone_module(&sh, &body, &iface);
                    dhcook::sm3::validate(&m).map(|_| ()).map_err(|e| format!("naga: {e}
{m}"))
                });
                match res {
                    Ok(()) => st.1 += 1,
                    Err(msg) => {
                        st.2 += 1;
                        let key: String = msg.lines().next().unwrap_or("").chars().take(100).collect();
                        let n = first_err.entry(key).or_default();
                        if *n < 2 {
                            fails.push_str(&format!("==== {} {:02x?}
{msg}
", r.ty, r.guid));
                        }
                        *n += 1;
                    }
                }
            }
            let (mut t, mut ok) = (0, 0);
            for (ty, (n, k, _)) in &stats {
                t += n;
                ok += k;
                if k < n {
                    println!("{ty}: {k}/{n}");
                }
            }
            println!("total {ok}/{t} valid");
            for (k, n) in &first_err {
                println!("{n:6} {k}");
            }
            std::fs::write("cache/debug/sm3_fail.txt", fails)?;
        }
        Some("sm3dump") => {
            // dhtool sm3dump <type filter> [n] [ctab]: WGSL of the n-th matching shader, or with
            // "ctab" a census of constant-table names over all matches
            let pkg = upk::Package::open(&pkg_path("RefShaderCache-PC-D3D-SM3"))?;
            let idx = pkg.exports_of_class("ShaderCache").next().expect("no ShaderCache");
            let e = &pkg.exports[idx as usize - 1];
            let data = pkg.export_data(idx)?;
            let recs = dhcook::shadercache::records(data, &pkg.names, e.offset)?;
            let filter = &args[2];
            // the n-th match, or "has:<ctab name>" for the first one using that constant
            let has = args.get(3).and_then(|s| s.strip_prefix("has:")).map(|s| s.to_string());
            let nth: usize = if has.is_some() { 0 } else { args.get(3).map(|s| s.parse()).transpose()?.unwrap_or(0) };
            let census = args.get(4).map(|s| s == "ctab").unwrap_or(false);
            let mut names: std::collections::BTreeMap<String, u32> = Default::default();
            let mut k = 0;
            for r in recs.iter().filter(|r| r.ty == *filter || (filter.ends_with('*') && r.ty.starts_with(&filter[..filter.len() - 1]))) {
                let sh = dhcook::sm3::parse(&data[r.code.clone()])?;
                if census {
                    for c in &sh.ctab {
                        *names.entry(format!("{} [{}]", c.name, ["bool", "int", "float", "sampler"][c.set.min(3) as usize])).or_default() += 1;
                    }
                    k += 1;
                    continue;
                }
                if let Some(h) = &has {
                    if !sh.ctab.iter().any(|c| c.name == *h) {
                        continue;
                    }
                }
                if k == nth {
                    println!("// {} {:02x?}", r.ty, r.guid);
                    for c in &sh.ctab {
                        println!("// ctab {} set {} reg {} count {}", c.name, c.set, c.reg, c.count);
                    }
                    for d in &sh.decls {
                        println!("// dcl ty {} reg {} usage {} index {} tex {} mask {:x}", d.ty, d.reg, d.usage, d.index, d.tex, d.mask);
                    }
                    let (body, iface) = dhcook::sm3::translate(&sh, "sm3_")?;
                    println!("{}", dhcook::sm3::standalone_module(&sh, &body, &iface));
                    break;
                }
                k += 1;
            }
            if census {
                println!("{k} shaders");
                let mut v: Vec<_> = names.into_iter().collect();
                v.sort_by(|a, b| b.1.cmp(&a.1));
                for (n, c) in v {
                    println!("{c:6} {n}");
                }
            }
        }
        Some("shadertypes") => {
            // dhtool shadertypes: how many shader maps have each (vertex factory, shader type)
            let lib = dhcook::shadercache::CookedLibrary::read(std::path::Path::new("cache/shaders.bin"))?;
            let mut count: std::collections::BTreeMap<(String, String), u32> = Default::default();
            for m in &lib.maps {
                for (vf, list) in &m.mesh_maps {
                    for (t, _) in list {
                        *count.entry((vf.clone(), t.clone())).or_default() += 1;
                    }
                }
            }
            println!("{} maps", lib.maps.len());
            for ((vf, t), n) in count {
                println!("{n:6} {vf} {t}");
            }
        }
        Some("ue3progs") => {
            // dhtool ue3progs [map index] [out.wgsl]: assemble the base pass programs of every
            // shader map (local vertex factory, each light-map policy) in standalone form and
            // validate them; with an index, write that map's program
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            let lib = dhcook::shader_library(&assets).expect("no shader cache");
            let cooked = dhcook::shadercache::CookedLibrary::from_library(lib);
            let only: Option<usize> = args.get(2).map(|s| s.parse()).transpose()?;
            let (mut ok, mut bad, mut missing) = (0, 0, 0);
            let mut unknown: std::collections::BTreeMap<String, u32> = Default::default();
            let mut errors: std::collections::BTreeMap<String, u32> = Default::default();
            let mut fails = String::new();
            for (mi, map) in cooked.maps.iter().enumerate() {
                if only.is_some_and(|o| o != mi) {
                    continue;
                }
                for policy in [dhcook::ue3prog::Policy::DirectionalLightMap, dhcook::ue3prog::Policy::NoLightMap] {
                    let (vst, pst) = policy.shader_types();
                    let (Some(vg), Some(pg)) = (map.shader("FLocalVertexFactory", vst), map.shader("FLocalVertexFactory", pst)) else {
                        missing += 1;
                        continue;
                    };
                    let (Some(vc), Some(pc)) = (cooked.code.get(&vg), cooked.code.get(&pg)) else {
                        missing += 1;
                        continue;
                    };
                    let platform = if args.get(3).is_some() { dhcook::ue3prog::Platform::Bevy } else { dhcook::ue3prog::Platform::Standalone };
                    let sun = (|| {
                        let (lv, lp) = dhcook::ue3prog::Policy::SUN;
                        Some((cooked.code.get(&map.shader("FLocalVertexFactory", lv)?)?.as_slice(), cooked.code.get(&map.shader("FLocalVertexFactory", lp)?)?.as_slice()))
                    })();
                    match dhcook::ue3prog::build(map, vc, pc, sun, platform) {
                        Ok(p) => {
                            for u in &p.unknown {
                                *unknown.entry(u.clone()).or_default() += 1;
                            }
                            if let Some(out) = args.get(3) {
                                std::fs::write(out, &p.wgsl)?;
                                println!("wrote {out} ({} {:?})", map.name, policy);
                                return Ok(());
                            }
                            match dhcook::sm3::validate(&p.wgsl) {
                                Ok(_) => ok += 1,
                                Err(e) => {
                                    bad += 1;
                                    let key: String = e.lines().next().unwrap_or("").chars().take(120).collect();
                                    let n = errors.entry(key).or_default();
                                    if *n < 1 {
                                        fails.push_str(&format!("==== map {mi} {} {:?}\n{e}\n{}\n", map.name, policy, p.wgsl));
                                    }
                                    *n += 1;
                                }
                            }
                        }
                        Err(e) => {
                            bad += 1;
                            *errors.entry(format!("build: {e}")).or_default() += 1;
                        }
                    }
                }
            }
            println!("programs: {ok} valid, {bad} invalid, {missing} without shaders");
            for (k, n) in &errors {
                println!("{n:6} {k}");
            }
            println!("unknown constants:");
            for (k, n) in &unknown {
                println!("{n:6} {k}");
            }
            std::fs::write("cache/debug/ue3prog_fail.txt", fails)?;
        }
        Some("meshinfo") => {
            // dhtool meshinfo <cooked .mesh>: attribute counts
            let m = dhcook::format::MeshFile::read(std::path::Path::new(&args[2]))?;
            println!(
                "positions {} normals {} tangents {} uv0 {} uv1 {} colors {} joints {} sections {:?}",
                m.positions.len(), m.normals.len(), m.tangents.len(), m.uv0.len(), m.uv1.len(), m.colors.len(), m.joints.len(), m.sections
            );
            // each section's joints (by the weight they carry) and its extent
            if !m.joints.is_empty() {
                for (si, (first, count)) in m.sections.iter().enumerate() {
                    let mut w: std::collections::BTreeMap<u16, f32> = Default::default();
                    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                    for &i in &m.indices[*first as usize..(*first + *count) as usize] {
                        let v = i as usize;
                        for k in 0..4 {
                            *w.entry(m.joints[v][k]).or_default() += m.weights[v][k];
                        }
                        for a in 0..3 {
                            lo[a] = lo[a].min(m.positions[v][a]);
                            hi[a] = hi[a].max(m.positions[v][a]);
                        }
                    }
                    let mut top: Vec<_> = w.into_iter().filter(|x| x.1 > 0.0).collect();
                    top.sort_by(|a, b| b.1.total_cmp(&a.1));
                    top.truncate(5);
                    println!("  section {si}: {} tris, joints {:?}, box {:?}..{:?}", count / 3, top, lo, hi);
                }
            }
        }
        Some("cook-audio") => {
            // dhtool cook-audio: every Wwise event and its media into cache/audio
            let t = std::time::Instant::now();
            let idx = dhcook::audio::cook(std::path::Path::new(COOKED), std::path::Path::new("cache"), &|f, m| eprintln!("[{:3.0}%] {m}", f * 100.0))?;
            println!("{} events, {} media in {:.1}s", idx.events.len(), idx.media.len(), t.elapsed().as_secs_f32());
        }
        Some("audioevent") => {
            // dhtool audioevent <name>: what an event plays
            let w = dhcook::audio::Wwise::load(std::path::Path::new(COOKED))?;
            let id = args[2].parse::<u32>().unwrap_or_else(|_| dhcook::audio::fnv(&args[2]));
            println!("{:#x} {:?}", id, w.event(id));
        }
        Some("cook-ui") => {
            // dhtool cook-ui: fonts (and later the Scaleform art) into cache/ui
            let root = std::path::Path::new("cache");
            for p in dhcook::cook_ui(std::path::Path::new(COOKED), root)? {
                println!("{}", p.display());
            }
        }
        Some("cook-gamedata") => {
            // dhtool cook-gamedata: powers, attributes, charms, upgrades, stores -> cache/game
            init_classes();
            let root = std::path::Path::new("cache");
            dhcook::gamedata::cook(std::path::Path::new(COOKED), root)?;
        }
        Some("swfdump") => {
            // dhtool swfdump <pkg> <export> <out>: a SwfMovie's raw GFx data
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            let od = upk::read_object(&pkg, idx)?;
            let (c, off, _) = od.props.array("RawData").expect("no RawData");
            std::fs::write(&args[4], &pkg.data[off..off + c])?;
        }
        Some("fnvnames") => {
            // dhtool fnvnames <hex hash>...: names in any package whose Wwise id (FNV) is one of these
            let want: Vec<u32> = args[2..].iter().filter_map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).collect();
            let mut seen = std::collections::HashSet::new();
            for e in std::fs::read_dir(COOKED)?.flatten() {
                let p = e.path();
                if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("upk") || x.eq_ignore_ascii_case("u")) {
                    continue;
                }
                let Ok(pkg) = upk::Package::open(&p) else { continue };
                for n in &pkg.names {
                    if seen.insert(n.to_ascii_lowercase()) && want.contains(&dhcook::audio::fnv(n)) {
                        println!("{:#010x} {} ({})", dhcook::audio::fnv(n), n, p.file_name().unwrap().to_string_lossy());
                    }
                }
            }
        }
        Some("findname") => {
            // dhtool findname <pkg> <name>: exports whose data holds the name (an FName: its index
            // then number 0), with the offsets
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let Some(ni) = pkg.names.iter().position(|n| n.eq_ignore_ascii_case(&args[3])) else { anyhow::bail!("no such name") };
            let mut pat = (ni as i32).to_le_bytes().to_vec();
            pat.extend_from_slice(&0i32.to_le_bytes());
            for i in 1..=pkg.exports.len() as i32 {
                let Ok(d) = pkg.export_data(i) else { continue };
                let hits: Vec<usize> = d.windows(8).enumerate().filter(|(_, w)| *w == pat.as_slice()).map(|(k, _)| k).collect();
                if !hits.is_empty() {
                    println!("{:6} [{}] {} at {:?}", i, pkg.class_name(i), pkg.obj_path(i), hits);
                }
            }
        }
        Some("findprop") => {
            // dhtool findprop <pkg> <property>: exports that set a property
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for i in 1..=pkg.exports.len() as i32 {
                let Ok(od) = upk::read_object(&pkg, i) else { continue };
                // Static-array overrides may only set a nonzero index (for example
                // bolt ammunition uses m_AmmoRanges[2]); get() only checks index zero.
                if od.props.0.iter().any(|p| p.name == args[3]) {
                    println!("{:6} [{}] {}", i, pkg.class_name(i), pkg.obj_path(i));
                    let one = upk::Props(od.props.0.iter().filter(|p| p.name == args[3]).cloned().collect());
                    dump_props(&pkg, &one, 1);
                }
            }
        }
        Some("audionode") => {
            // dhtool audionode <id|name> [depth]: a Wwise object and what it references
            let w = dhcook::audio::Wwise::load(std::path::Path::new(COOKED))?;
            let id = args[2].parse::<u32>().unwrap_or_else(|_| dhcook::audio::fnv(&args[2]));
            let depth: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(2);
            fn show(w: &dhcook::audio::Wwise, id: u32, d: usize, max: usize) {
                let pad = "  ".repeat(d);
                let Some((kind, children, actions, data)) = w.node_debug(id) else {
                    println!("{pad}{id:#x} (missing)");
                    return;
                };
                println!("{pad}{id:#x} kind {kind} children {:x?} actions {:x?} ({} bytes)", children, actions, data.len());
                for row in data.chunks(32).take(std::env::var("DH_ROWS").ok().and_then(|s| s.parse().ok()).unwrap_or(12)) {
                    println!("{pad}  {}", row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
                }
                if d < max {
                    for c in actions.iter().chain(children.iter()) {
                        show(w, *c, d + 1, max);
                    }
                }
            }
            show(&w, id, 0, depth);
        }
        Some("posdump") => {
            // dhtool posdump: sound objects' bytes from their positioning on, a sample of each
            // leading pattern, the failures first
            let w = dhcook::audio::Wwise::load(std::path::Path::new(COOKED))?;
            let mut v = w.positioning_bytes();
            v.sort_by_key(|x| (x.2, x.3.get(..4).map(|b| b.to_vec())));
            let mut shown: std::collections::HashMap<(bool, Vec<u8>), usize> = Default::default();
            for (id, kind, ok, b) in &v {
                let key = (*ok, b.iter().take(if b.first() == Some(&0) { 1 } else { 3 }).copied().collect::<Vec<u8>>());
                let c = shown.entry(key).or_default();
                *c += 1;
                if *c <= 3 {
                    println!("{} {id:#010x} k{kind} {}", if *ok { "ok  " } else { "FAIL" }, b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
                }
            }
            let mut keys: Vec<_> = shown.into_iter().collect();
            keys.sort();
            for ((ok, k), c) in keys {
                println!("{} {:02x?}: {c}", if ok { "ok" } else { "FAIL" }, k);
            }
        }
        Some("rtpcrejects") => {
            // dhtool rtpcrejects <kind>: objects whose NodeBaseParams the RTPC reader rejects
            let w = dhcook::audio::Wwise::load(std::path::Path::new(COOKED))?;
            let mut v = w.rtpc_rejects(args[2].parse()?);
            v.sort_by_key(|x| x.3.len());
            for (id, start, end, d) in v.iter().step_by((v.len() / 8).max(1)).take(8) {
                println!("{id:#010x} start {start} end {end:?} len {}", d.len());
                for row in d.chunks(32).take(4) {
                    println!("  {}", row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
                }
            }
        }
        Some("rtpcs") => {
            // dhtool rtpcs [name]: the RTPC curves on the sound objects (of one game parameter)
            let w = dhcook::audio::Wwise::load(std::path::Path::new(COOKED))?;
            for (kind, nine, eight, none) in w.rtpc_coverage() {
                println!("NodeBaseParams kind {kind}: {nine} with 9-byte advanced settings, {eight} with 8, {none} unread");
            }
            let only = args.get(2).map(|s| s.parse::<u32>().unwrap_or_else(|_| dhcook::audio::fnv(s)));
            let mut by: std::collections::BTreeMap<(u32, u32), Vec<(f32, f32, u8)>> = Default::default();
            for (id, kind, curves) in w.rtpc_curves() {
                for c in curves {
                    let (lo, hi) = c.points.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
                    by.entry((c.rtpc, c.param)).or_default().push((lo, hi, c.scaling));
                    if only == Some(c.rtpc) {
                        println!("{id:#x} kind {kind} param {} scaling {} {:?}", c.param, c.scaling, c.points);
                    }
                }
            }
            if only.is_none() {
                for ((r, p), v) in &by {
                    let lo = v.iter().map(|x| x.0).fold(f32::MAX, f32::min);
                    let hi = v.iter().map(|x| x.1).fold(f32::MIN, f32::max);
                    let sc: std::collections::BTreeSet<u8> = v.iter().map(|x| x.2).collect();
                    println!("rtpc {r:#010x} param {p:2}: {} curves, y {lo:.2}..{hi:.2} scaling {sc:?}", v.len());
                }
            }
        }
        Some("cook-shaders") => {
            // dhtool cook-shaders: rewrite cache/shaders.bin
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            dhcook::cook_shader_library(&assets, std::path::Path::new("cache"), true)?;
        }
        Some("gsdump") => {
            // dhtool gsdump [name]: list the global shader cache, or translate one shader
            let d = std::fs::read(std::path::Path::new(COOKED).join("GlobalShaderCache-PC-D3D-SM3.bin"))?;
            let shaders = dhcook::shadercache::global_shaders(&d)?;
            match args.get(2) {
                None => {
                    for g in &shaders {
                        println!("{} freq {} {} bytes", g.name, g.frequency, g.code.len());
                    }
                }
                Some(n) => {
                    for g in shaders.iter().filter(|g| g.name == *n) {
                        let sh = dhcook::sm3::parse(&g.code)?;
                        println!("// {} {:02x?}", g.name, g.guid);
                        for c in &sh.ctab {
                            println!("// ctab {} set {} reg {} count {}", c.name, c.set, c.reg, c.count);
                        }
                        for dc in &sh.decls {
                            println!("// dcl ty {} reg {} usage {} index {} tex {} mask {:x}", dc.ty, dc.reg, dc.usage, dc.index, dc.tex, dc.mask);
                        }
                        let (body, iface) = dhcook::sm3::translate(&sh, "sm3_")?;
                        println!("{}", dhcook::sm3::standalone_module(&sh, &body, &iface));
                    }
                }
            }
        }
        Some("mattex") => {
            // dhtool mattex <pkg> <material export>: the textures its compiled resource samples
            // (`UniformExpressionTextures`, in the shader's Texture2D_N order)
            let pkg = std::sync::Arc::new(upk::Package::open(&pkg_path(&args[2]))?);
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            let o = dhcook::resolver::Obj { pkg: pkg.clone(), idx };
            match dhcook::materials::material_resource(&o) {
                Some(r) => {
                    for (i, t) in r.textures.iter().enumerate() {
                        println!("Texture2D_{i}: {}", if *t != 0 { pkg.obj_path(*t) } else { "-".into() });
                    }
                }
                None => println!("no material resource"),
            }
        }
        Some("postshader") => {
            // dhtool postshader <name part|map index> [print]: post-process materials as
            // full-screen passes (`ue3prog::build_post`), validated with naga
            let lib = dhcook::shadercache::CookedLibrary::read(&std::path::Path::new("cache").join("shaders.bin"))?;
            let ty = "TPpMaterialPixelShader<FPpMaterialLinearSpaceMeshPolicy>";
            let part = args[2].to_ascii_lowercase();
            for (i, m) in lib.maps.iter().enumerate() {
                if part.parse::<usize>().ok() != Some(i) && !m.name.to_ascii_lowercase().contains(&part) {
                    continue;
                }
                let Some(g) = m.shader("FLocalVertexFactory", ty) else { continue };
                let Some(code) = lib.code.get(&g) else { continue };
                match dhcook::ue3prog::build_post(m, code) {
                    Ok(p) => {
                        let v = dhcook::sm3::validate(&p.wgsl);
                        println!("map {i} {}: {} unknown {:?}", m.name, if v.is_ok() { "ok" } else { "INVALID" }, p.unknown);
                        if let Err(e) = v {
                            println!("{e}");
                        }
                        if args.get(3).map(|s| s.as_str()) == Some("print") {
                            println!("{}", p.wgsl);
                        }
                    }
                    Err(e) => println!("map {i} {}: {e:#}", m.name),
                }
            }
        }
        Some("enum") => {
            // dhtool enum <pkg> <enum name...>: an enum's values, in order
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for name in &args[3..] {
                println!("{name}: {:?}", dhcook::gamedata::enum_names(&pkg, name));
            }
        }
        Some("findmap") => {
            // dhtool findmap <name part>: the cooked shader maps of materials whose name holds
            // it, with their shaders (non-mesh ones, and the vertex factories of mesh ones)
            let lib = dhcook::shadercache::CookedLibrary::read(&std::path::Path::new("cache").join("shaders.bin"))?;
            let part = args[2].to_ascii_lowercase();
            for (i, m) in lib.maps.iter().enumerate() {
                if !m.name.to_ascii_lowercase().contains(&part) {
                    continue;
                }
                println!("map {i}: {} ({} params)", m.name, dhcook::shadercache::param_nodes(m).len());
                for (t, g) in &m.shaders {
                    println!("    {t} ({} bytes)", lib.code.get(g).map(|c| c.len()).unwrap_or(0));
                }
                for (vf, list) in &m.mesh_maps {
                    println!("    {vf}: {}", list.iter().map(|(t, _)| t.as_str()).filter(|t| t.contains("Pp") || t.contains("BasePassPixel")).collect::<Vec<_>>().join(", "));
                }
            }
        }
        Some("mapshader") => {
            // dhtool mapshader <map index> [vertex factory part] [shader type part]: a cooked
            // material shader map's shaders, or one translated to WGSL
            let lib = dhcook::shadercache::CookedLibrary::read(&std::path::Path::new("cache").join("shaders.bin"))?;
            let i: usize = args[2].parse()?;
            let m = &lib.maps[i];
            println!("// map {i}: {}", m.name);
            if args.get(3).map(|s| s.as_str()) == Some("exprs") {
                for (k, e) in m.pixel.vectors.iter().enumerate() { println!("pixel vector {k}: {e:?}"); }
                for (k, e) in m.pixel.scalars.iter().enumerate() { println!("pixel scalar {k}: {e:?}"); }
                for (k, e) in m.pixel.textures.iter().enumerate() { println!("pixel texture {k}: {e:?}"); }
                return Ok(());
            }
            let (vfw, tyw) = (args.get(3).cloned().unwrap_or_default(), args.get(4).cloned());
            // (vertex factory "-": the map's non-mesh shaders)
            let plain = [("-".to_string(), m.shaders.clone())];
            let lists: &[(String, Vec<(String, [u8; 16])>)] = if vfw == "-" { &plain } else { &m.mesh_maps };
            for (vf, list) in lists {
                if !vf.contains(vfw.as_str()) {
                    continue;
                }
                for (ty, g) in list {
                    match &tyw {
                        None => println!("{vf} {ty}"),
                        Some(t) if ty.contains(t.as_str()) => {
                            let Some(code) = lib.code.get(g) else { continue };
                            let sh = dhcook::sm3::parse(code)?;
                            println!("// {vf} {ty}");
                            for c in &sh.ctab {
                                println!("// ctab {} set {} reg {} count {}", c.name, c.set, c.reg, c.count);
                            }
                            for dc in &sh.decls {
                                println!("// dcl ty {} reg {} usage {} index {} tex {} mask {:x}", dc.ty, dc.reg, dc.usage, dc.index, dc.tex, dc.mask);
                            }
                            let (body, iface) = dhcook::sm3::translate(&sh, "sm3_")?;
                            println!("{}", dhcook::sm3::standalone_module(&sh, &body, &iface));
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("findguid") => {
            // dhtool findguid <pkg> <32 hex digits as printed by the cooker>: exports containing it
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let h = &args[3];
            let mut needle = Vec::new();
            for k in 0..4 {
                let v = u32::from_str_radix(&h[k * 8..k * 8 + 8], 16)?;
                needle.extend_from_slice(&v.to_le_bytes());
            }
            for i in 1..=pkg.exports.len() as i32 {
                let e = &pkg.exports[i as usize - 1];
                let d = &pkg.data[e.offset..(e.offset + e.size).min(pkg.data.len())];
                if d.windows(16).any(|w| w == needle.as_slice()) {
                    println!("{i} [{}] {}", pkg.class_name(i), pkg.obj_path(i));
                }
            }
        }
        Some("refs") => {
            // dhtool refs <pkg> <export path or index>: objects whose properties reference it
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let target: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            fn scan(pkg: &upk::Package, props: &upk::Props, target: i32, path: &str, out: &mut Vec<String>) {
                for p in &props.0 {
                    match &p.value {
                        upk::Value::Object(o) if *o == target => out.push(format!("{path}.{}", p.name)),
                        upk::Value::Struct(_, sub) => scan(pkg, &upk::Props(sub.clone()), target, &format!("{path}.{}", p.name), out),
                        upk::Value::Array { offset, size, .. } => {
                            let d = &pkg.data[*offset..*offset + *size];
                            if d.chunks(4).any(|c| c.len() == 4 && i32::from_le_bytes([c[0], c[1], c[2], c[3]]) == target) {
                                out.push(format!("{path}.{}[?]", p.name));
                            }
                        }
                        _ => {}
                    }
                }
            }
            for i in 1..=pkg.exports.len() as i32 {
                let Ok(od) = upk::read_object(&pkg, i) else { continue };
                let mut out = Vec::new();
                scan(&pkg, &od.props, target, "", &mut out);
                if !out.is_empty() {
                    println!("{i} [{}] {} {:?}", pkg.class_name(i), pkg.obj_path(i), out);
                }
            }
        }
        Some("kismetstats") => {
            // dhtool kismetstats: sequence op / variable classes over every map's levels
            init_classes();
            let cooked = std::path::PathBuf::from(COOKED);
            let mut counts: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
            for m in dhcook::list_maps(&cooked) {
                let Ok(p) = upk::Package::open(&pkg_path(&m)) else { continue };
                let mut names = vec![m.clone()];
                names.extend(dhcook::streaming_levels(&p));
                let mut seen_in_map = std::collections::HashSet::new();
                for n in names {
                    let Ok(pkg) = upk::Package::open(&pkg_path(&n)) else { continue };
                    for i in 1..=pkg.exports.len() as i32 {
                        let c = pkg.class_name(i);
                        if c.contains("Seq") && !c.contains("Property") && c != "Class" {
                            let e = counts.entry(c.clone()).or_default();
                            e.0 += 1;
                            if seen_in_map.insert(c) { e.1 += 1; }
                        }
                    }
                }
            }
            let mut v: Vec<_> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
            for (c, (n, maps)) in v { println!("{n:6} {maps:3} {c}"); }
        }
        Some("edgedump") => {
            // dhtool edgedump <blob>...: decoded keys as JSON lines (for validation)
            for f in &args[2..] {
                let a = dhcook::edge::EdgeAnim::decode(&std::fs::read(f)?)?;
                let tr = |v: &Vec<dhcook::edge::Track<[f32; 4]>>| v.iter().map(|t| format!("[{},{:?},{:?}]", t.joint, t.frames, t.values)).collect::<Vec<_>>().join(",");
                let tt = |v: &Vec<dhcook::edge::Track<[f32; 3]>>| v.iter().map(|t| format!("[{},{:?},{:?}]", t.joint, t.frames, t.values)).collect::<Vec<_>>().join(",");
                println!("{{\"file\":{:?},\"frames\":{},\"r\":[{}],\"t\":[{}],\"s\":[{}]}}", f, a.num_frames, tr(&a.rotations), tt(&a.translations), tt(&a.scales));
            }
        }
        Some("edgeskel") => {
            // dhtool edgeskel <pkg> <mesh name substring> <out file>: dump a mesh's Edge skeleton
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for i in pkg.exports_of_class("SkeletalMesh").collect::<Vec<_>>() {
                if !pkg.obj_path(i).contains(&args[3]) { continue; }
                let m = dhcook::skeletal::read_skeletal_mesh(&pkg, i)?;
                println!("{} bones={} edge skeleton={} bytes", pkg.obj_path(i), m.bones.len(), m.edge_skeleton.len());
                std::fs::write(&args[4], &m.edge_skeleton)?;
                let names: Vec<String> = m.bones.iter().map(|b| format!("{} {} {:?} {:?}", b.name, b.parent, b.rotation, b.position)).collect();
                std::fs::write(format!("{}.bones.txt", args[4]), names.join("
"))?;
                break;
            }
        }
        Some("parseall") => {
            init_classes();
            for name in &args[2..] {
                let pkg = upk::Package::open(&pkg_path(name))?;
                let mut fails = std::collections::BTreeMap::new();
                let mut n = 0;
                for i in 1..=pkg.exports.len() as i32 {
                    let cls = pkg.class_name(i);
                    if cls == "Class" || pkg.exports[i as usize - 1].size == 0 { continue; }
                    // skip script/struct objects (not property-serialized the same way)
                    if matches!(cls.as_str(), "Function" | "State" | "ScriptStruct" | "Enum" | "Const" | "Package" | "TextBuffer") || cls.ends_with("Property") { continue; }
                    n += 1;
                    if let Err(e) = upk::read_object(&pkg, i) {
                        let ent = fails.entry(cls.clone()).or_insert((0, String::new()));
                        ent.0 += 1;
                        if ent.1.is_empty() { ent.1 = format!("{} : {e:#}", pkg.obj_path(i)); }
                    }
                }
                println!("== {name}: {n} objects, {} failing classes", fails.len());
                for (k, (c, ex)) in fails { println!("  {c:5} {k}  e.g. {ex}"); }
            }
        }
        Some("texfile") => {
            let tf = dhcook::format::TexFile::read(std::path::Path::new(&args[2]))?;
            use dhcook::format::TexFormat as F;
            let pf = match tf.format { F::Bc1 => upk::texture::PixelFormat::Dxt1, F::Bc2 => upk::texture::PixelFormat::Dxt3, F::Bc3 => upk::texture::PixelFormat::Dxt5, F::Bc5 => upk::texture::PixelFormat::Bc5, F::R8 => upk::texture::PixelFormat::G8, _ => upk::texture::PixelFormat::Bgra8 };
            let rgba = if tf.format == F::Rgba8 { tf.mips[0].clone() } else { dhcook::decode_rgba(pf, tf.width, tf.height, &tf.mips[0])? };
            image::save_buffer(&args[3], &rgba, tf.width, tf.height, image::ColorType::Rgba8)?;
            println!("{:?} {}x{} mips={}", tf.format, tf.width, tf.height, tf.mips.len());
        }
        Some("smca") => {
            // dhtool smca <pkg>: collection components with their own transform props
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            for a in pkg.exports_of_class("StaticMeshCollectionActor").collect::<Vec<_>>() {
                let od = upk::read_object(&pkg, a)?;
                let comps = upk::props::object_array(&pkg, &od.props, "StaticMeshComponents");
                let mut r = od.reader;
                for c in comps {
                    let mut m = [0f32; 16];
                    for x in m.iter_mut() { *x = r.f32()?; }
                    if c <= 0 { continue; }
                    let cp = upk::read_object(&pkg, c)?.props;
                    let s3 = cp.vector("Scale3D");
                    let s = cp.float("Scale");
                    let t = cp.vector("Translation");
                    let rot = cp.get("Rotation").is_some();
                    if s3.is_none() && s.is_none() && t.is_none() && !rot { continue; }
                    let len = |i: usize| (m[i * 4] * m[i * 4] + m[i * 4 + 1] * m[i * 4 + 1] + m[i * 4 + 2] * m[i * 4 + 2]).sqrt();
                    let mesh = cp.object("StaticMesh").map(|o| pkg.obj_path(o)).unwrap_or_default();
                    println!("{} {mesh} s3={s3:?} s={s:?} t={t:?} rot={rot} | matrix row scales {:.3} {:.3} {:.3}", pkg.obj_path(c), len(0), len(1), len(2));
                }
            }
        }
        Some("statics") => {
            // dhtool statics <pkgs> <material instance>: scan native data for static switch entries
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            assets.init_class_registry();
            assets.load_globals();
            for name in args[2].split(',') {
                if let Some(p) = assets.package(name) {
                    assets.add_search(p);
                }
            }
            let mut cur = assets.find(&args[3]);
            while let Some(o) = cur.take() {
                let od = upk::read_object(&o.pkg, o.idx)?;
                let e = &o.pkg.exports[o.idx as usize - 1];
                let (start, end) = (od.reader.pos, e.offset + e.size);
                let d = &o.pkg.data;
                println!("== {} ({}): native {} bytes", o.path(), o.pkg.name, end - start);
                let mut i = start;
                while i + 16 <= end {
                    let idx = i32::from_le_bytes(d[i..i + 4].try_into().unwrap());
                    let num = i32::from_le_bytes(d[i + 4..i + 8].try_into().unwrap());
                    if idx > 0 && (idx as usize) < o.pkg.names.len() && num == 0 {
                        let n = &o.pkg.names[idx as usize];
                        if n != "None" && !n.starts_with("Pt_") {
                            let v = u32::from_le_bytes(d[i + 8..i + 12].try_into().unwrap());
                            let ov = u32::from_le_bytes(d[i + 12..i + 16].try_into().unwrap());
                            println!("  @{} {n} value={v} override={ov}", i - start);
                            i += 8;
                            continue;
                        }
                    }
                    i += 4;
                }
                cur = od.props.object("Parent").and_then(|p| assets.resolve(&o.pkg, p));
            }
        }
        Some("animset") => {
            // dhtool animset <pkg> <AnimSet export> <outdir>: dump every sequence's Edge blob
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            let out = std::path::PathBuf::from(&args[4]);
            std::fs::create_dir_all(&out)?;
            let od = upk::read_object(&pkg, idx)?;
            let mut bones = Vec::new();
            if let Some(upk::Value::Array { count, offset, .. }) = od.props.get("TrackBoneNames") {
                let mut r = upk::Reader::at(&pkg.data, *offset);
                for _ in 0..*count { bones.push(pkg.read_name(&mut r)?); }
            }
            std::fs::write(out.join("bones.txt"), bones.join("
"))?;
            for s in upk::props::object_array(&pkg, &od.props, "Sequences") {
                let so = upk::read_object(&pkg, s)?;
                let name = so.props.name("SequenceName").unwrap_or("?").to_string();
                let frames = so.props.get("NumFrames").and_then(|v| if let upk::Value::Int(i) = v { Some(*i) } else { None }).unwrap_or(0);
                let len = so.props.float("SequenceLength").unwrap_or(0.0);
                let mut r = so.reader;
                let raw = r.i32()?;
                let n = r.i32()? as usize;
                let blob = r.bytes(n)?.to_vec();
                println!("{s} {name} frames={frames} len={len} raw={raw} blob={n}");
                std::fs::write(out.join(format!("{name}.bin")), &blob)?;
            }
        }
        Some("graph") => {
            // dhtool graph <pkg> <material export> [input]: material expression tree
            init_classes();
            let pkg = upk::Package::open(&pkg_path(&args[2]))?;
            let idx: i32 = match args[3].parse() { Ok(i) => i, Err(_) => pkg.find_export(&args[3]).expect("no such export") };
            let props = upk::read_object(&pkg, idx)?.props;
            let roots: Vec<String> = match args.get(4) {
                Some(r) => vec![r.clone()],
                None => props.0.iter().filter(|p| matches!(p.value, upk::Value::Struct(..))).map(|p| p.name.clone()).collect(),
            };
            for r in roots {
                if let Some(upk::Value::Struct(_, sub)) = props.get(&r) {
                    println!("{r}:");
                    graph_input(&pkg, &upk::Props(sub.clone()), 1, &mut Vec::new());
                }
            }
        }
        Some("mat") => {
            // dhtool mat <map> <material path>: resolved parameters of a material
            let assets = dhcook::resolver::Assets::new(std::path::Path::new(COOKED))?;
            assets.init_class_registry();
            assets.load_globals();
            for name in args[2].split(',') {
                if let Some(p) = assets.package(name) {
                    assets.add_search(p);
                }
            }
            let o = assets.find(&args[3]).ok_or_else(|| anyhow::anyhow!("not found"))?;
            if let Some(path) = args.get(4) {
                // dhtool mat <pkgs> <material> <object>: locate any object and list its children
                let t = assets.find(path).ok_or_else(|| anyhow::anyhow!("object not found"))?;
                println!("{} in package {} (export {})", t.path(), t.pkg.name, t.idx);
                for i in 1..=t.pkg.exports.len() as i32 {
                    if t.pkg.obj_outer(i) == t.idx {
                        println!("  {:6} {:40} {}", i, t.pkg.class_name(i), t.pkg.obj_path(i));
                    }
                }
                return Ok(());
            }
            let rm = dhcook::materials::resolve_material(&assets, &o);
            println!("chain: {:?}\nblend {:?} two_sided {} unlit {}", rm.chain, rm.blend, rm.two_sided, rm.unlit);
            for (n, t) in &rm.textures { println!("  tex {n} = {}", t.path()); }
            for (n, v) in &rm.vectors { println!("  vec {n} = {v:?}"); }
            for (n, v) in &rm.scalars { println!("  scalar {n} = {v}"); }
            for t in &rm.unnamed { println!("  unnamed {}", t.path()); }
            for (n, v) in &rm.switches { if *v { println!("  switch {n}"); } }
            for (n, v) in &rm.masks { println!("  mask {n} = {v:?}"); }
        }
        _ => eprintln!("usage: dhtool list|imports|classes|hex <pkg> ..."),
    }
    Ok(())
}

fn dump_props(pkg: &upk::Package, props: &upk::Props, depth: usize) {
    let ind = "  ".repeat(depth);
    for p in &props.0 {
        let v = match &p.value {
            upk::Value::Object(o) => format!("{} ({})", pkg.obj_path(*o), o),
            upk::Value::Struct(n, sub) => {
                println!("{ind}{}[{}] {}: struct {} {{", p.name, p.index, p.ty, n);
                dump_props(pkg, &upk::Props(sub.clone()), depth + 1);
                println!("{ind}}}");
                continue;
            }
            upk::Value::Array { count, offset, size } => {
                // arrays of tagged structs: expand when they parse cleanly
                if *count > 0 && *size >= count * 9 && depth < 8 {
                    if let Ok(items) = upk::props::parse_struct_array(pkg, *offset, *size, *count) {
                        if items.iter().all(|i| !i.0.is_empty()) {
                            println!("{ind}{}[{}] {}: array[{count}] of struct", p.name, p.index, p.ty);
                            for (k, it) in items.iter().enumerate() {
                                println!("{ind}  [{k}] {{");
                                dump_props(pkg, it, depth + 2);
                                println!("{ind}  }}");
                            }
                            continue;
                        }
                    }
                }
                let d = &pkg.data[*offset..*offset + (*size).min(48)];
                format!("array[{count}] {size}b {:02x?}", d)
            }
            other => format!("{:?}", other),
        };
        println!("{ind}{}[{}] {}: {}", p.name, p.index, p.ty, v);
    }
}

fn decode_rgba(f: upk::texture::PixelFormat, w: u32, h: u32, d: &[u8]) -> Vec<u8> {
    use upk::texture::PixelFormat as P;
    let mut out = vec![0u8; (w * h * 4) as usize];
    let fmt = match f {
        P::Dxt1 => Some(texpresso::Format::Bc1),
        P::Dxt3 => Some(texpresso::Format::Bc2),
        P::Dxt5 => Some(texpresso::Format::Bc3),
        P::Bc5 => Some(texpresso::Format::Bc5),
        _ => None,
    };
    if let Some(fmt) = fmt {
        fmt.decompress(d, w as usize, h as usize, &mut out);
    } else if f == P::Bgra8 {
        for (i, c) in d.chunks(4).enumerate().take((w * h) as usize) { out[i*4..i*4+4].copy_from_slice(&[c[2], c[1], c[0], c[3]]); }
    } else if f == P::G8 {
        for (i, c) in d.iter().enumerate().take((w * h) as usize) { out[i*4..i*4+4].copy_from_slice(&[*c, *c, *c, 255]); }
    }
    out
}

fn init_classes() {
    let pk: Vec<upk::Package> = ["Core", "Engine", "GameFramework", "DishonoredGame", "AkAudio", "GFxUI"]
        .iter()
        .filter_map(|n| upk::Package::open(&pkg_path(n)).ok())
        .collect();
    let refs: Vec<&upk::Package> = pk.iter().collect();
    let set = upk::props::component_classes_from(&refs);
    log::info!("{} component classes", set.len());
    upk::props::set_component_classes(set);
}

fn graph_input(pkg: &upk::Package, input: &upk::Props, depth: usize, stack: &mut Vec<i32>) {
    let ind = "  ".repeat(depth);
    let Some(e) = input.object("Expression") else { return };
    let mask: Vec<String> = ["MaskR", "MaskG", "MaskB", "MaskA"]
        .iter()
        .filter(|m| matches!(input.get(m), Some(upk::Value::Int(1)) | Some(upk::Value::Bool(true))))
        .map(|m| m[4..].to_string())
        .collect();
    let cls = pkg.class_name(e).replace("MaterialExpression", "");
    if stack.contains(&e) || depth > 40 {
        println!("{ind}{cls} #{e} (cycle/deep)");
        return;
    }
    let Ok(od) = upk::read_object(pkg, e) else { println!("{ind}{cls} #{e} <unreadable>"); return };
    let mut info = Vec::new();
    for p in &od.props.0 {
        match (&p.name[..], &p.value) {
            (_, upk::Value::Struct(n, _)) if n == "ExpressionInput" => {}
            ("ParameterName", v) | ("ExpressionGUID", v) if matches!(v, upk::Value::Struct(..)) => {}
            ("ParameterName", upk::Value::Name(n)) => info.push(format!("'{n}'")),
            ("Texture", upk::Value::Object(o)) => info.push(format!("tex={}", pkg.obj_path(*o))),
            ("Material", _) | ("EditorX", _) | ("EditorY", _) | ("ExpressionGUID", _) | ("Desc", _) | ("bIsParameterExpression", _) => {}
            (n, v) => info.push(format!("{n}={v:?}")),
        }
    }
    println!("{ind}{cls}{} #{e} {}", if mask.is_empty() { String::new() } else { format!(".{}", mask.join("")) }, info.join(" "));
    stack.push(e);
    for p in &od.props.0 {
        if let upk::Value::Struct(n, sub) = &p.value {
            if n == "ExpressionInput" {
                let sp = upk::Props(sub.clone());
                if sp.object("Expression").is_some() {
                    println!("{ind}  [{}]", p.name);
                    graph_input(pkg, &sp, depth + 2, stack);
                }
            }
        }
    }
    stack.pop();
}
