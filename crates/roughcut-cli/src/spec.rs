//! Every command, declared once.
//!
//! The command line and the MCP server are two ways of reaching this table and
//! nothing else. A tool schema, a `--flag` parser, the usage text and the
//! dispatch all read the same entries, so a command cannot exist on one front
//! door and not the other, and its parameters cannot be spelled differently in
//! the two places.

use crate::args::Args;
use crate::ops;
use anyhow::Result;
use roughcut_core::model::Project;
use serde_json::Value;
use std::path::Path;

/// What a parameter accepts. Deliberately few: this is the whole type system
/// the two front doors have to agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// A frame number, or any other whole number. Positions are frames
    /// everywhere in Roughcut — never seconds and never timecode. Output
    /// carries timecode beside the frame for reading; input never accepts it,
    /// because two spellings of a position is how a cut ends up a frame out.
    Int,
    Bool,
    Path,
    /// One of a fixed set of words.
    Choice(&'static [&'static str]),
}

#[derive(Debug)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// May be given more than once, arriving as a list.
    pub many: bool,
    pub help: &'static str,
}

/// Shorthand, so the table below reads as a table.
const fn p(name: &'static str, kind: Kind, required: bool, help: &'static str) -> Param {
    Param { name, kind, required, many: false, help }
}

const fn many(name: &'static str, kind: Kind, required: bool, help: &'static str) -> Param {
    Param { name, kind, required, many: true, help }
}

/// Everything a command needs when it works on the project in memory.
pub struct Ctx<'a> {
    pub project: &'a mut Project,
    pub args: &'a Args,
    /// Where the project came from, for messages and for the scratch files an
    /// export or a render puts beside it.
    pub path: &'a Path,
}

#[derive(Debug)]
pub enum Body {
    /// Reads or edits the loaded project. The runner loads once, calls this,
    /// and saves afterwards if `writes` is set — which is what makes a batch
    /// of twenty edits one atomic write rather than twenty.
    OnProject(fn(&mut Ctx) -> Result<Value>),
    /// Needs no project, or does its own file handling.
    Standalone(fn(&Args) -> Result<Value>),
}

#[derive(Debug)]
pub struct Cmd {
    pub name: &'static str,
    pub help: &'static str,
    pub params: &'static [Param],
    /// Changes the project, so it is written back. Also what tells an MCP
    /// client whether the tool only reads.
    pub writes: bool,
    pub body: Body,
}

/// The `--project` parameter, on every command that has one. Optional because
/// `ROUGHCUT_PROJECT` can supply it: an agent making thirty edits in a row
/// should not have to repeat the path thirty times, and an MCP server can be
/// launched pinned to one project.
const PROJECT: Param = p(
    "project",
    Kind::Path,
    false,
    "The .roughcut project. Defaults to $ROUGHCUT_PROJECT.",
);

const CLIP: Param = p(
    "clip",
    Kind::Text,
    true,
    "A bin clip: its id, or any part of its file name that matches exactly one clip.",
);

const RIPPLE: Param = p(
    "ripple",
    Kind::Choice(&["picture", "all"]),
    false,
    "Whether sound on the audio tracks shifts with the picture. Default picture.",
);

pub const COMMANDS: &[Cmd] = &[
    // -- the project itself --------------------------------------------------
    Cmd {
        name: "new",
        help: "Create an empty project. The first import sets its format.",
        params: &[
            p("project", Kind::Path, true, "Where to write the .roughcut file."),
            p("force", Kind::Bool, false, "Overwrite an existing project."),
        ],
        writes: false,
        body: Body::Standalone(ops::new_project),
    },
    Cmd {
        name: "info",
        help: "The format, bin size, timeline length and audio tracks.",
        params: &[PROJECT],
        writes: false,
        body: Body::OnProject(ops::info),
    },
    Cmd {
        name: "doctor",
        help: "Where the external tools are: ffprobe, ffmpeg, melt, whisper and its model.",
        params: &[],
        writes: false,
        body: Body::Standalone(ops::doctor),
    },
    Cmd {
        name: "batch",
        help: "Run several project commands as one edit, written back once. Takes a JSON \
               array of steps, each an object with a command name and its parameters. \
               Either every step applies or none does.",
        params: &[
            PROJECT,
            p("steps", Kind::Text, true,
              "JSON array, e.g. [{\"command\":\"append\",\"clip\":\"IMG_01\"}]."),
        ],
        writes: true,
        body: Body::OnProject(ops::batch),
    },

    // -- the bin -------------------------------------------------------------
    Cmd {
        name: "import",
        help: "Probe files and add them to the bin. A directory brings in every media file \
               under it. Files already imported are reported, not duplicated.",
        params: &[
            PROJECT,
            many("path", Kind::Path, true, "A file or directory. Repeatable."),
        ],
        writes: true,
        body: Body::OnProject(ops::import),
    },
    Cmd {
        name: "clips",
        help: "The bin: id, file, length, marks, flag.",
        params: &[
            PROJECT,
            p("flagged", Kind::Bool, false, "Only clips flagged, or only clips not."),
            p("match", Kind::Text, false, "Only clips whose file name contains this."),
        ],
        writes: false,
        body: Body::OnProject(ops::clips),
    },
    Cmd {
        name: "mark",
        help: "Set a clip in and out point, in frames. Omitting one leaves it alone.",
        params: &[
            PROJECT,
            CLIP,
            p("in", Kind::Int, false, "First frame, inclusive."),
            p("out", Kind::Int, false, "Last frame, inclusive."),
            p("clear", Kind::Bool, false, "Remove both marks instead."),
        ],
        writes: true,
        body: Body::OnProject(ops::mark),
    },
    Cmd {
        name: "flag",
        help: "Flag a clip as worth using, or unflag it. Culling is a pass of its own.",
        params: &[PROJECT, CLIP, p("off", Kind::Bool, false, "Unflag instead.")],
        writes: true,
        body: Body::OnProject(ops::flag),
    },
    Cmd {
        name: "remove-clip",
        help: "Drop a clip from the bin. Refused while the timeline still uses it.",
        params: &[PROJECT, CLIP],
        writes: true,
        body: Body::OnProject(ops::remove_clip),
    },
    Cmd {
        name: "relink",
        help: "Point a bin clip at a file that has moved.",
        params: &[PROJECT, CLIP, p("to", Kind::Path, true, "The file new location.")],
        writes: true,
        body: Body::OnProject(ops::relink),
    },
    Cmd {
        name: "missing",
        help: "Bin clips whose source file is no longer where it was.",
        params: &[PROJECT],
        writes: false,
        body: Body::OnProject(ops::missing),
    },
    Cmd {
        name: "probe",
        help: "What ffprobe says about a file, without importing it.",
        params: &[p("path", Kind::Path, true, "The file to read.")],
        writes: false,
        body: Body::Standalone(ops::probe),
    },
    Cmd {
        name: "rotate",
        help: "Turn a clip file a quarter turn, in place. This rewrites the source on disk; \
               marks and cuts survive because the frame count does not change.",
        params: &[
            PROJECT,
            CLIP,
            p("turn", Kind::Choice(&["cw", "ccw"]), false, "Direction. Default cw."),
        ],
        writes: true,
        body: Body::OnProject(ops::rotate),
    },

    // -- the timeline --------------------------------------------------------
    Cmd {
        name: "timeline",
        help: "The cut: every item with its index, source range, start and length.",
        params: &[PROJECT],
        writes: false,
        body: Body::OnProject(ops::timeline),
    },
    Cmd {
        name: "append",
        help: "Append a clip marked range to the end of the timeline.",
        params: &[
            PROJECT,
            CLIP,
            p("in", Kind::Int, false, "Override the clip mark in."),
            p("out", Kind::Int, false, "Override the clip mark out."),
        ],
        writes: true,
        body: Body::OnProject(ops::append),
    },
    Cmd {
        name: "insert",
        help: "Insert a clip marked range at a frame, pushing the rest later. Splits \
               whatever is already there.",
        params: &[
            PROJECT,
            CLIP,
            p("at", Kind::Int, true, "Timeline frame to insert at."),
            p("in", Kind::Int, false, "Override the clip mark in."),
            p("out", Kind::Int, false, "Override the clip mark out."),
            RIPPLE,
        ],
        writes: true,
        body: Body::OnProject(ops::insert),
    },
    Cmd {
        name: "split",
        help: "Split the timeline item under a frame into two.",
        params: &[PROJECT, p("at", Kind::Int, true, "Timeline frame to cut at.")],
        writes: true,
        body: Body::OnProject(ops::split),
    },
    Cmd {
        name: "delete",
        help: "Remove a timeline item and close the gap.",
        params: &[
            PROJECT,
            p("index", Kind::Int, true, "Position in the timeline, from 0."),
            RIPPLE,
        ],
        writes: true,
        body: Body::OnProject(ops::delete),
    },
    Cmd {
        name: "trim",
        help: "Move one edge of a timeline item. A positive amount moves it later.",
        params: &[
            PROJECT,
            p("index", Kind::Int, true, "Position in the timeline, from 0."),
            p("edge", Kind::Choice(&["head", "tail"]), true, "Which end to move."),
            p("by", Kind::Int, true, "Frames to move it by. Clamped to what the source has."),
            RIPPLE,
        ],
        writes: true,
        body: Body::OnProject(ops::trim),
    },
    Cmd {
        name: "move",
        help: "Reorder the timeline: take the item at one index and put it at another.",
        params: &[
            PROJECT,
            p("index", Kind::Int, true, "Position to take from, from 0."),
            p("to", Kind::Int, true, "Position to put it at."),
        ],
        writes: true,
        body: Body::OnProject(ops::move_item),
    },
    Cmd {
        name: "clear",
        help: "Empty the timeline, leaving the bin alone.",
        params: &[PROJECT],
        writes: true,
        body: Body::OnProject(ops::clear),
    },

    // -- audio tracks --------------------------------------------------------
    Cmd {
        name: "audio",
        help: "The audio tracks and what is on them.",
        params: &[PROJECT],
        writes: false,
        body: Body::OnProject(ops::audio),
    },
    Cmd {
        name: "add-audio-track",
        help: "Add an audio track for music, voiceover or effects.",
        params: &[
            PROJECT,
            p("name", Kind::Text, false, "What to call it. Default A1, A2, and so on."),
        ],
        writes: true,
        body: Body::OnProject(ops::add_audio_track),
    },
    Cmd {
        name: "remove-audio-track",
        help: "Remove an audio track and everything on it.",
        params: &[PROJECT, p("track", Kind::Int, true, "Track number, from 0.")],
        writes: true,
        body: Body::OnProject(ops::remove_audio_track),
    },
    Cmd {
        name: "place-audio",
        help: "Put a clip sound on an audio track at a frame, overwriting what is under it.",
        params: &[
            PROJECT,
            CLIP,
            p("track", Kind::Int, true, "Track number, from 0."),
            p("at", Kind::Int, true, "Timeline frame to start at."),
            p("in", Kind::Int, false, "Override the clip mark in."),
            p("out", Kind::Int, false, "Override the clip mark out."),
        ],
        writes: true,
        body: Body::OnProject(ops::place_audio),
    },
    Cmd {
        name: "remove-audio",
        help: "Take one piece off an audio track.",
        params: &[
            PROJECT,
            p("track", Kind::Int, true, "Track number, from 0."),
            p("index", Kind::Int, true, "Position on that track, from 0."),
        ],
        writes: true,
        body: Body::OnProject(ops::remove_audio),
    },
    Cmd {
        name: "mute",
        help: "Mute an audio track, or unmute it.",
        params: &[
            PROJECT,
            p("track", Kind::Int, true, "Track number, from 0."),
            p("off", Kind::Bool, false, "Unmute instead."),
        ],
        writes: true,
        body: Body::OnProject(ops::mute),
    },

    // -- words ---------------------------------------------------------------
    Cmd {
        name: "transcript",
        help: "A clip transcript, if one has been made. Words carry the frame they are \
               spoken on, so a range of words is a range of the cut.",
        params: &[
            PROJECT,
            CLIP,
            p("words", Kind::Bool, false, "Include every word with its timing, not just the text."),
        ],
        writes: false,
        body: Body::OnProject(ops::transcript),
    },
    Cmd {
        name: "transcribe",
        help: "Make transcripts with whisper and cache them. Slow — tens of seconds a clip — \
               and blocks until done. Clips that already have one are skipped.",
        params: &[
            PROJECT,
            p("clip", Kind::Text, false, "One clip. Omit to do the whole bin."),
            p("limit", Kind::Int, false, "Stop after this many clips."),
            p("force", Kind::Bool, false, "Redo clips that already have one."),
        ],
        writes: false,
        body: Body::OnProject(ops::transcribe),
    },
    Cmd {
        name: "search",
        help: "Find spoken words across every transcript already made. Each hit carries the \
               frame range to cut and the word indices to hand to cut-words.",
        params: &[
            PROJECT,
            p("query", Kind::Text, true, "Words to look for. Case is ignored."),
            p("clip", Kind::Text, false, "Only search this clip."),
            p("limit", Kind::Int, false, "Most hits to return. Default 20."),
        ],
        writes: false,
        body: Body::OnProject(ops::search),
    },
    Cmd {
        name: "cut-words",
        help: "Put exactly what is said between two words of a clip transcript on the \
               timeline. The frame range comes from the word timings, padded a little at \
               each end so the cut does not clip the speech.",
        params: &[
            PROJECT,
            CLIP,
            p("from", Kind::Int, true, "First word index, from search or transcript --words."),
            p("to", Kind::Int, true, "Last word index, inclusive."),
            p("at", Kind::Int, false, "Insert at this timeline frame instead of appending."),
        ],
        writes: true,
        body: Body::OnProject(ops::cut_words),
    },

    // -- getting it out ------------------------------------------------------
    Cmd {
        name: "export",
        help: "Write the cut as MLT XML, sized and timed from the clips used. This is the \
               file Shotcut opens.",
        params: &[
            PROJECT,
            p("out", Kind::Path, true, "Where to write the .mlt file."),
            p("title", Kind::Text, false, "Project title in the XML. Default Roughcut."),
        ],
        writes: false,
        body: Body::OnProject(ops::export),
    },
    Cmd {
        name: "render",
        help: "Encode the cut to an MP4 with melt. Takes minutes and blocks until finished; \
               progress goes to standard error.",
        params: &[PROJECT, p("out", Kind::Path, true, "Where to write the .mp4.")],
        writes: false,
        body: Body::OnProject(ops::render),
    },
];

pub fn find(name: &str) -> Option<&'static Cmd> {
    COMMANDS.iter().find(|c| c.name == name)
}

impl Cmd {
    pub fn param(&self, name: &str) -> Option<&'static Param> {
        self.params.iter().find(|p| p.name == name)
    }

    /// The command help, as a person would want to read it.
    pub fn usage(&self) -> String {
        let mut out = format!("roughcut-cli {}\n\n{}\n", self.name, wrap(self.help, 76));
        if self.params.is_empty() {
            return out;
        }
        out.push_str("\nParameters:\n");
        let width = self.params.iter().map(|p| p.name.len()).max().unwrap_or(0);
        for p in self.params {
            let tag = match p.kind {
                Kind::Bool => String::new(),
                Kind::Choice(all) => format!(" <{}>", all.join("|")),
                _ => " <value>".to_string(),
            };
            out.push_str(&format!(
                "  --{:<width$}{}{}  {}\n",
                p.name,
                tag,
                if p.required { "  (required)" } else { "" },
                p.help,
                width = width
            ));
        }
        out
    }
}

/// The whole surface, one line a command.
pub fn usage() -> String {
    let mut out = String::from(
        "roughcut-cli — drive Roughcut without the window.\n\n\
         Usage:  roughcut-cli <command> [--name value ...]\n\
         \x20       roughcut-cli help <command>\n\
         \x20       roughcut-cli mcp                serve these commands over MCP\n\n\
         Positions are frame numbers, never seconds or timecode. Output is JSON.\n\
         $ROUGHCUT_PROJECT supplies --project when it is not given.\n\n\
         Commands:\n",
    );
    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in COMMANDS {
        out.push_str(&format!("  {:width$}  {}\n", c.name, first_sentence(c.help), width = width));
    }
    out
}

/// The first sentence of a help string, with its line breaks flattened.
pub fn first_sentence(help: &str) -> String {
    let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(at) => flat[..=at].to_string(),
        None => flat,
    }
}

fn wrap(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = 0;
    for word in text.split_whitespace() {
        if line > 0 && line + 1 + word.len() > width {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += word.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_is_uniquely_named_and_documented() {
        let mut seen = HashSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.name), "two commands called {}", c.name);
            assert!(!c.help.is_empty(), "{} has no help", c.name);
            assert!(
                c.name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '-'),
                "{} is not a plain lower-case name, which an MCP tool name has to be",
                c.name
            );
            let mut params = HashSet::new();
            for p in c.params {
                assert!(params.insert(p.name), "{} declares {} twice", c.name, p.name);
                assert!(!p.help.is_empty(), "{}.{} has no help", c.name, p.name);
            }
        }
    }

    /// A command that edits the project but is never written back would look
    /// like it worked and change nothing.
    #[test]
    fn only_project_commands_claim_to_write() {
        for c in COMMANDS {
            if c.writes {
                assert!(
                    matches!(c.body, Body::OnProject(_)),
                    "{} says it writes but does not take the project",
                    c.name
                );
            }
        }
    }

    #[test]
    fn anything_taking_a_project_offers_the_project_parameter() {
        for c in COMMANDS {
            if matches!(c.body, Body::OnProject(_)) {
                assert!(c.param("project").is_some(), "{} cannot say which project", c.name);
            }
        }
    }

    #[test]
    fn usage_names_every_command() {
        let text = usage();
        for c in COMMANDS {
            assert!(text.contains(c.name), "usage omits {}", c.name);
            assert!(!c.usage().is_empty());
        }
    }
}
