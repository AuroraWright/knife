//! kdu: knife data utility. Inspect and extract game data.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use knife_core::format::{archive::Archive, bntx, mzx, nx, script};

#[derive(Parser)]
struct Cli {
    /// Path to the extracted romfs directory
    #[arg(long, default_value = "switch_romfs")]
    romfs: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List entries of an archive (allpac, allui, ...)
    List { archive: String },
    /// Dump all scene scripts as text
    Scenes { out: PathBuf },
    /// Print one message line
    Line { index: usize },
    /// Dump every script text line (index<TAB>text)
    Lines,
    /// Load executable tables and print a summary
    Tables {
        #[arg(long, default_value = "switch_exefs/uncompressed_main")]
        exe: PathBuf,
        /// Print every image record
        #[arg(long)]
        all: bool,
    },
    /// Parse every scene and report command counts
    ParseAll,
    /// Flowchart node starts: JSON {node: [scene, pc, scene name, offset, next offset]} of the first RDST of each
    /// node (offsets as save records store them)
    Nodes,
    /// List stream sounds (BGM) in the BFSAR
    Streams,
    /// Decode a sound (BFSAR wave sound name or stream/*.opus file) to WAV
    Sound { name: String, out: PathBuf },
    /// Extract an archive entry, converting BNTX textures to PNG (BC7 decoded)
    Extract { archive: String, name: String, out: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List { archive } => {
            let a = Archive::open(&cli.romfs, &archive)?;
            for (i, e) in a.entries().iter().enumerate() {
                println!("{i:6} {:<32} {:#12x} {:6} {:6}", e.name, e.offset, e.stored_sectors, e.unpacked_sectors);
            }
        }
        Cmd::Scenes { out } => {
            let data = std::fs::read(cli.romfs.join("allscr.mrg"))?;
            let s = script::SceneArchive::parse(&data)?;
            std::fs::create_dir_all(&out)?;
            for i in 0..s.scene_count() {
                let name = s.names.get(i).cloned().unwrap_or_else(|| format!("scene{i}"));
                std::fs::write(out.join(format!("{i:03}_{name}.txt")), s.scene(i).unwrap())?;
            }
            println!("{} scenes, {} names", s.scene_count(), s.names.len());
        }
        Cmd::Tables { exe, all } => {
            let nso = knife_core::exe::Nso::parse(std::fs::read(exe)?)?;
            let t = knife_core::exe::ExeTables::load(&nso)?;
            println!("{} images, {} scene ids", t.images.len(), t.scene_ids.len());
            if all {
                for im in &t.images {
                    println!("{} {} {} {} {} {}", im.name, im.width, im.height, im.center_x, im.center_y, im.kind);
                }
            } else {
                for i in [0usize, 604, 2352, 10780] {
                    println!("{:?}", t.images[i]);
                }
            }
        }
        Cmd::Nodes => {
            let data = std::fs::read(cli.romfs.join("allscr.mrg"))?;
            let s = script::SceneArchive::parse(&data)?;
            let mut nodes = std::collections::BTreeMap::new();
            for i in 0..s.scene_count() {
                let sc = knife_core::vm::parse::Scene::parse(s.scene(i).unwrap());
                for (pc, c) in sc.commands.iter().enumerate() {
                    if c.name == "RDST"
                        && let Some(n) = c.int(0)
                    {
                        // save records address commands by the offset of the `;` before them and the next one
                        let next = sc.commands.get(pc + 1).map_or(c.offset + 5, |c| c.offset);
                        nodes.entry(n).or_insert((i, pc, c.offset - 1, next));
                    }
                }
            }
            let body: Vec<String> = nodes
                .iter()
                .map(|(n, (i, pc, off, next))| {
                    format!("\"{n}\": [{i}, {pc}, \"{}\", {off}, {next}]", s.names.get(*i).map_or("", |n| n.as_str()))
                })
                .collect();
            println!("{{{}}}", body.join(", "));
        }
        Cmd::ParseAll => {
            let data = std::fs::read(cli.romfs.join("allscr.mrg"))?;
            let s = script::SceneArchive::parse(&data)?;
            let (mut cmds, mut msgs, mut labels) = (0, 0, 0);
            for i in 0..s.scene_count() {
                let sc = knife_core::vm::parse::Scene::parse(s.scene(i).unwrap());
                cmds += sc.commands.iter().filter(|c| matches!(c.op, knife_core::vm::parse::Op::Cmd(_))).count();
                msgs += sc.commands.iter().filter(|c| c.op == knife_core::vm::parse::Op::Message).count();
                labels += sc.labels.len();
            }
            println!("commands {cmds}, messages {msgs}, labels {labels}");
        }
        Cmd::Streams => {
            let sar = knife_audio::bfsar::SoundArchive::parse(std::fs::read(cli.romfs.join("knife1.bfsar"))?)?;
            for i in 0..sar.sound_count() {
                match sar.stream_sound(i) {
                    Ok(s) => println!("{i:5} {:?}", s),
                    Err(_) => {}
                }
            }
        }
        Cmd::Sound { name, out } => {
            let pcm = if name.ends_with(".opus") {
                knife_audio::codec::decode_nopus(&std::fs::read(cli.romfs.join("stream").join(&name))?)?
            } else {
                let sar = knife_audio::bfsar::SoundArchive::parse(std::fs::read(cli.romfs.join("knife1.bfsar"))?)?;
                sar.load_wave_sound(&name)?
            };
            println!("{} Hz, {} frames, loop {:?}", pcm.sample_rate, pcm.frames.len(), pcm.loop_range);
            let mut w = Vec::new();
            let n = pcm.frames.len() as u32;
            w.extend_from_slice(b"RIFF");
            w.extend_from_slice(&(36 + n * 4).to_le_bytes());
            w.extend_from_slice(b"WAVEfmt ");
            w.extend_from_slice(&16u32.to_le_bytes());
            w.extend_from_slice(&[1, 0, 2, 0]);
            w.extend_from_slice(&pcm.sample_rate.to_le_bytes());
            w.extend_from_slice(&(pcm.sample_rate * 4).to_le_bytes());
            w.extend_from_slice(&[4, 0, 16, 0]);
            w.extend_from_slice(b"data");
            w.extend_from_slice(&(n * 4).to_le_bytes());
            for f in &pcm.frames {
                for s in f {
                    w.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
            }
            std::fs::write(out, w)?;
        }
        Cmd::Line { index } => {
            let data = std::fs::read(cli.romfs.join("script_text.mrg"))?;
            let t = script::ScriptText::parse(&data, 0)?;
            println!("{}", t.get(index).context("no such line")?);
        }
        Cmd::Lines => {
            let data = std::fs::read(cli.romfs.join("script_text.mrg"))?;
            let t = script::ScriptText::parse(&data, 0)?;
            for i in 0..t.len() {
                println!("{i}\t{}", t.get(i).unwrap_or(""));
            }
        }
        Cmd::Extract { archive, name, out } => {
            let a = Archive::open(&cli.romfs, &archive)?;
            // `#N` selects an entry by index (archives without a name table, e.g. parts)
            let data = match name.strip_prefix('#').and_then(|n| n.parse().ok()) {
                Some(i) => a.read(i)?,
                None => a.read_by_name(&name)?,
            };
            if bntx::is_bntx(&data) {
                for (i, t) in bntx::parse(&data)?.into_iter().enumerate() {
                    let path = out.with_file_name(format!(
                        "{}_{i}_{}.png",
                        out.file_stem().unwrap().to_string_lossy(),
                        t.name
                    ));
                    println!("{} {:?} {}x{}", t.name, t.format, t.width, t.height);
                    let rgba = knife_core::format::bntx_decode::to_rgba8(&t)?;
                    image::save_buffer(&path, &rgba, t.width, t.height, image::ExtendedColorType::Rgba8)?;
                }
            } else {
                let _ = (mzx::is_mzx(&data), nx::kind(&data));
                std::fs::write(&out, &data)?;
            }
        }
    }
    Ok(())
}
