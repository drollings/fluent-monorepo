//! Session presets: dependency graphs over pipeline stages.
//!
//! A preset names the nodes for one session kind with the assets each
//! needs and provides. Startup order is the topological sort; shutdown is
//! its reverse; failure containment walks transitive dependents. The graph
//! encodes ordering and containment, never dataflow.

use fluent_dag::dep_graph::{DependencyGraph, GraphError};

/// Dictation preset: microphone to focused window. The `aec` node joins
/// when speakers are active; the graph stays acyclic either way.
pub fn dictation_preset(with_aec: bool) -> Result<DependencyGraph<&'static str>, GraphError> {
    let mut graph = DependencyGraph::new();
    graph.register(&"capture.mic", &[], &["pcm.mic"])?;
    if with_aec {
        graph.register(&"aec", &["pcm.mic"], &["pcm.mic.clean"])?;
        graph.register(&"vad.mic", &["pcm.mic.clean"], &["vad.mic"])?;
        graph.register(
            &"stt.mic",
            &["pcm.mic.clean", "vad.mic"],
            &["text.user"],
        )?;
    } else {
        graph.register(&"vad.mic", &["pcm.mic"], &["vad.mic"])?;
        graph.register(&"stt.mic", &["pcm.mic", "vad.mic"], &["text.user"])?;
    }
    graph.register(&"inject", &["text.user"], &["desktop.text"])?;
    Ok(graph)
}

/// Assistant preset: the full duplex cascade. Playback stays
/// infrastructure (the audio scope's sink), never a session node, so a
/// TTS failure cannot cancel the sink.
pub fn assistant_preset() -> Result<DependencyGraph<&'static str>, GraphError> {
    let mut graph = DependencyGraph::new();
    graph.register(&"capture.mic", &[], &["pcm.mic"])?;
    graph.register(&"aec", &["pcm.mic"], &["pcm.mic.clean"])?;
    graph.register(&"vad.mic", &["pcm.mic.clean"], &["vad.mic"])?;
    graph.register(
        &"stt.mic",
        &["pcm.mic.clean", "vad.mic"],
        &["text.user"],
    )?;
    graph.register(&"turn", &["vad.mic", "text.user"], &["turn.commit"])?;
    graph.register(&"llm", &["turn.commit"], &["text.reply"])?;
    graph.register(&"tools", &["text.reply"], &["tool.result"])?;
    graph.register(&"tts", &["text.reply"], &["pcm.reply"])?;
    graph.register(&"transcript", &["text.user"], &["transcript.live"])?;
    Ok(graph)
}

/// Meeting preset: two-channel capture with archive, two STT lanes,
// and the merged live transcript. No `tts` node, so speaking is
/// structurally impossible; the archive node is always present, so a
/// crash loses at most the open segment.
pub fn meeting_preset() -> Result<DependencyGraph<&'static str>, GraphError> {
    let mut graph = DependencyGraph::new();
    graph.register(&"capture.mic", &[], &["pcm.mic"])?;
    graph.register(&"capture.sys", &[], &["pcm.sys"])?;
    graph.register(&"archive", &["pcm.mic", "pcm.sys"], &["audio.archive"])?;
    graph.register(&"vad.mic", &["pcm.mic"], &["vad.mic"])?;
    graph.register(&"vad.sys", &["pcm.sys"], &["vad.sys"])?;
    graph.register(&"stt.mic", &["pcm.mic", "vad.mic"], &["text.user"])?;
    graph.register(&"stt.sys", &["pcm.sys", "vad.sys"], &["text.remote"])?;
    graph.register(
        &"transcript",
        &["text.user", "text.remote"],
        &["transcript.live"],
    )?;
    graph.register(&"notes.live", &["transcript.live"], &["notes.live"])?;
    Ok(graph)
}

/// Duplex preset: the native speech model collapses the cascade into
/// one `duplex` node fed by mic capture. Archive, transcript, and
/// injection ride along unchanged — only the inference path differs.
pub fn duplex_preset() -> Result<DependencyGraph<&'static str>, GraphError> {
    let mut graph = DependencyGraph::new();
    graph.register(&"capture.mic", &[], &["pcm.mic"])?;
    graph.register(&"archive", &["pcm.mic"], &["audio.archive"])?;
    graph.register(&"duplex", &["pcm.mic"], &["text.reply", "pcm.reply"])?;
    graph.register(&"transcript", &["pcm.mic"], &["transcript.live"])?;
    graph.register(&"inject", &["transcript.live"], &["desktop.text"])?;
    Ok(graph)
}

#[cfg(test)]
#[path = "../tests/preset.rs"]
mod tests;
