//! SSE text must survive arbitrary transport chunk boundaries, including UTF-8 code points.

use kanon_llm::gateway::providers::SseDecoder;

#[test]
fn unicode_event_names_and_data_survive_every_chunk_boundary() {
    let wire = "event: 更新\r\ndata: {\"text\":\"你好🌍\"}\r\ndata: 后续\r\n\r\n\
                event: done\ndata: [DONE]\n\n";
    let expected: Vec<(Option<String>, String)> = vec![
        (Some("更新".into()), "{\"text\":\"你好🌍\"}\n后续".into()),
        (Some("done".into()), "[DONE]".into()),
    ];

    for split in 0..=wire.len() {
        let mut decoder = SseDecoder::new();
        let mut events = decoder.decode(&wire.as_bytes()[..split]);
        events.extend(decoder.decode(&wire.as_bytes()[split..]));
        let decoded: Vec<_> = events
            .into_iter()
            .map(|event| (event.event, event.data))
            .collect();
        assert_eq!(decoded, expected, "split at byte {split}");
    }

    let mut decoder = SseDecoder::new();
    let decoded: Vec<_> = wire
        .as_bytes()
        .chunks(1)
        .flat_map(|chunk| decoder.decode(chunk))
        .map(|event| (event.event, event.data))
        .collect();
    assert_eq!(decoded, expected, "one byte per transport chunk");
}
