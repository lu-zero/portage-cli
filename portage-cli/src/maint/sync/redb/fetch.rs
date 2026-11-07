//! A protocol-v2 fetch received into memory: no `.git`, no shallow file, no ref store
//!
//! The stored commit is sent as both `shallow` and `have`, so the server sends one tree's worth of
//! changes, deltified against objects the database already holds.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use gix::protocol::transport::client::blocking_io::{ExtendedBufRead, TransportV2Ext};
use gix::protocol::transport::{self, Protocol, Service};

/// What a fetch brought back
pub(super) struct Fetched {
    /// The remote's `HEAD`
    pub(super) tip: gix::ObjectId,
    /// The pack, empty when `tip` is the commit already held
    pub(super) pack: Vec<u8>,
}

/// Fetch the remote `HEAD` of `url` at depth 1, as a thin pack against `have` when there is one
pub(super) fn fetch(url: &str, have: Option<gix::ObjectId>) -> Result<Fetched> {
    let mut transport = transport::client::blocking_io::connect::connect(
        url,
        transport::client::blocking_io::connect::Options {
            version: Protocol::V2,
            ..Default::default()
        },
    )
    .map_err(|e| anyhow!("connecting to {url}: {e}"))?;
    let mut progress = gix::progress::Discard;
    let handshake = gix::protocol::handshake(
        &mut transport,
        Service::UploadPack,
        |_| Ok(None),
        vec![],
        &mut progress,
    )
    .map_err(|e| anyhow!("handshake with {url}: {e}"))?;
    if handshake.server_protocol_version != Protocol::V2 {
        bail!("{url} does not speak git protocol v2");
    }
    let agent: gix::protocol::command::Feature = (
        "agent",
        Some(gix::protocol::agent(concat!(
            "em/",
            env!("CARGO_PKG_VERSION")
        ))),
    );

    let mut prefixes = gix::protocol::ls_refs::RefPrefixes::new();
    prefixes.extend([gix::bstr::BString::from("HEAD")]);
    let refs =
        gix::protocol::LsRefsCommand::new(Some(prefixes), &handshake.capabilities, agent.clone())
            .invoke_blocking(&mut transport, &mut progress, false)
            .map_err(|e| anyhow!("listing the refs of {url}: {e}"))?;
    let tip = refs
        .iter()
        .find_map(|r| {
            let (name, target, peeled) = r.unpack();
            (name == "HEAD")
                .then(|| peeled.or(target).map(ToOwned::to_owned))
                .flatten()
        })
        .with_context(|| format!("{url} has no HEAD"))?;
    if Some(tip) == have {
        let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
        return Ok(Fetched {
            tip,
            pack: Vec::new(),
        });
    }

    // Only agent and object-format are capabilities: upload-pack dies on argument lines sent as
    // ones. The arguments are written here, since gix's own keep their shallow state in a file.
    let features: [gix::protocol::command::Feature; 2] =
        [agent, ("object-format", Some("sha1".into()))];
    let mut args = vec![
        "thin-pack".to_string(),
        "ofs-delta".into(),
        format!("want {tip}"),
    ];
    if let Some(have) = have {
        // Holding one commit and its tree, not its history: without `shallow` the server would
        // assume every ancestor is held and could leave out blobs that only older trees had.
        args.extend([format!("shallow {have}"), format!("have {have}")]);
    }
    args.extend(["deepen 1".into(), "done".into()]);

    let mut reader = transport
        .invoke(
            "fetch",
            features.iter().map(|(n, v)| (*n, v.as_deref())),
            Some(args.into_iter().map(Into::into)),
            false,
        )
        .map_err(|e| anyhow!("fetching from {url}: {e}"))?;
    let response =
        gix::protocol::fetch::Response::from_line_reader(Protocol::V2, &mut reader, true, false)
            .map_err(|e| anyhow!("reading the fetch response of {url}: {e}"))?;
    if !response.has_pack() {
        bail!("{url} sent no pack");
    }
    reader.set_progress_handler(Some(Box::new(|_, _| std::ops::ControlFlow::Continue(()))));
    let mut pack = Vec::new();
    reader
        .read_to_end(&mut pack)
        .with_context(|| format!("receiving the pack from {url}"))?;
    drop(reader);
    let _ = gix::protocol::indicate_end_of_interaction(&mut transport, false);
    Ok(Fetched { tip, pack })
}
