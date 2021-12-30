use async_std::task;
use std::convert::TryInto;
use std::fs;
use sqlx::query;
use tide::{Body, Request, StatusCode};
use tide::prelude::*;

use super::BeatSaverMap;
use super::{establish_connection, load_dats_for_analysis, num_to_key};

//async fn index(req: Request<()>) -> tide::Result {
//    let mut res: tide::Response = "\
//<html>
//<head>
//</head>
//<body>
//    test
//</body>
//</html>
//".into();
//    res.set_content_type(tide::http::Mime::from_str("text/html;charset=utf-8").unwrap());
//    Ok(res)
//}

async fn api(_req: Request<()>) -> tide::Result {
    let conn = &establish_connection();
    let results: Vec<_> = query!("
        SELECT s.key, sm.hash, sm.bsmeta
        FROM tSong s, tSongMeta sm, tSongData sd
        WHERE
            s.deleted = false AND
            s.key = sm.key AND
            sm.hash = sd.hash
        ORDER BY s.key DESC
        LIMIT 100
    ").fetch_all(conn).await.unwrap();
    let results: Vec<(String, String, String)> = results.into_iter()
        .map(|result| {
            let bsmeta: BeatSaverMap = serde_json::from_slice(&result.bsmeta).unwrap();
            (num_to_key(result.key), result.hash, format!("{} {}", bsmeta.metadata.song_name, bsmeta.metadata.song_sub_name))
        })
        .collect();
    Ok(Body::from_json(&results)?.into())
}

async fn submit(mut req: Request<()>) -> tide::Result {
    #[derive(Deserialize)]
    struct AnalysisSubmit {
        hash: String,
        interp: String,
        script: String,
    }
    let AnalysisSubmit { hash, interp, script } = req.body_json().await?;

    let (base_plugin, to_replace) = match interp.as_str() {
        "js" => ("parity", "script.js"),
        "py" => ("difficulty", "script.py"),
        _ => return Ok(StatusCode::NotFound.into()),
    };
    let plugin_path = format!("plugins/dist/{}.tar", base_plugin);
    let interp_path = format!("plugins/dist/{}.wasm", interp);
    let base_tar_data = fs::read(&plugin_path).unwrap();
    let mut tar_data = vec![];
    {
        let mut ar = tar::Builder::new(&mut tar_data);
        for entry in tar::Archive::new(&*base_tar_data).entries().unwrap() {
            println!("time for entry");
            let entry = entry.unwrap();
            let mut header = entry.header().to_owned();
            println!("cloned header {:?}", entry.path());
            if entry.path_bytes() == to_replace.as_bytes() {
                println!("appending header manufactured");
                header.set_size(script.as_bytes().len().try_into().unwrap());
                header.set_cksum();
                ar.append(&header, script.as_bytes())
            } else {
                println!("appending header");
                ar.append(&header, entry)
            }.unwrap();
            print!("completed entry");
        }
        ar.finish().unwrap();
    }
    let plugin = super::wasm::dynamic_plugin("dynamic", interp_path.as_ref(), tar_data).unwrap();
    let conn = &establish_connection();
    let dats = load_dats_for_analysis(conn, &hash);
    let ret = match plugin.run(dats) {
        Ok((stderr, Ok(d))) => format!("success: {}\n\nstderr:{{#\n{}\n#}}", serde_json::to_string(&d).unwrap(), stderr),
        Ok((stderr, Err(e))) => format!("error: {}\n\nstderr:{{#\n{}\n#}}", e, stderr),
        Err(e) => format!("{:?}", e),
    };
    Ok(ret.into())
}

pub fn serve() -> ! {
    task::block_on(async {
        //tide::log::with_level(tide::log::LevelFilter::Info);
        let mut app = tide::new();
        //app.at("/").get(index);
        app.at("/").serve_file("static/index.html").unwrap();
        app.at("/api").get(api);
        app.at("/submit").post(submit);
        //app.at("/src").serve_dir("src/")?;
        //app.at("/example").serve_file("examples/static_file.html")?;

        app.listen("0.0.0.0:8080").await.unwrap()
    });
    todo!()
}
