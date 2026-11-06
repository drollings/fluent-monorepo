#!/usr/bin/env python3
"""Record LFM3-Audio reference outputs for the orator-lfm fixtures.

Executes the real HuggingFace reference code (no reimplementation):
feature extractor -> mel frames -> encoder tokens, chat template
framing, tokenizer ids, codec frame counts, generation params. The
JSON printed on stdout is frozen into
src/orator-lfm/tests/fixtures/reference_expansion.json; the hermetic
test asserts our assembler against it byte-for-byte, and the live
test re-runs this probe to guard fixture staleness.

Usage: probe_reference.py [REFERENCE_DIR]
Requires: torch, transformers, numpy. Codec weights only for the
audio_out section (skipped with a notice when unloadable).
"""
import json
import os
import sys

REF_DIR = sys.argv[1] if len(sys.argv) > 1 else "/ai/models/lfm3/LiquidAI/LFM3-Audio-2B"

result: dict = {"reference_dir": REF_DIR, "fixtures": []}


def note(section: str, detail: str) -> None:
    result.setdefault("notes", []).append(f"{section}: {detail}")


def sine(samples: int, hz: float, rate: int):
    import numpy as np

    t = np.arange(samples, dtype=np.float64) / rate
    return (0.5 * np.sin(2.0 * np.pi * hz * t)).astype(np.float32)


def main() -> None:
    import numpy as np

    sys.path.insert(0, REF_DIR)
    sys.path.insert(0, "/tmp/opencode")  # lfmpkg: package view of the reference sources
    from feature_extraction_lfm3_audio import Lfm3AudioFeatureExtractor

    extractor = Lfm3AudioFeatureExtractor()
    result["mel_contract"] = {
        "feature_size": extractor.feature_size,
        "sampling_rate": extractor.sampling_rate,
        "n_fft": extractor.n_fft,
        "hop_length": extractor.hop_length,
        "win_length": extractor.win_length,
        "log_floor": extractor.log_floor,
        "normalization_offset": extractor.normalization_offset,
        "normalization_divisor": extractor.normalization_divisor,
        "encoder_stride": extractor.encoder_stride,
    }

    # 1. Mel frames + encoder tokens for fixed waveforms (no weights).
    for name, samples, hz in [
        ("edge-160-samples", 160, 440.0),
        ("one-second-16k", 16000, 440.0),
        ("three-seconds-16k", 48000, 880.0),
    ]:
        waveform = sine(samples, hz, 16000)
        out = extractor(waveform, sampling_rate=16000)
        frames = int(np.asarray(out["feature_attention_mask"]).sum())
        tokens = extractor.num_encoder_tokens(frames)
        result["fixtures"].append(
            {
                "name": name,
                "samples": samples,
                "mel_frames": frames,
                "encoder_tokens": tokens,
            }
        )

    # 2. Tokenizer ids + chat-template framing (no model weights).
    try:
        from transformers import AutoTokenizer

        tokenizer = AutoTokenizer.from_pretrained(REF_DIR, local_files_only=True, trust_remote_code=True)
        audio_id = tokenizer.convert_tokens_to_ids("<|audio|>")
        audio_out_id = tokenizer.convert_tokens_to_ids("<|audio_out|>")
        result["token_ids"] = {
            "audio": audio_id,
            "audio_out": audio_out_id,
            "bos": tokenizer.bos_token_id,
            "eos": tokenizer.eos_token_id,
            "pad": tokenizer.pad_token_id,
        }
    except Exception as exc:  # pragma: no cover - env dependent
        note("tokenizer", f"unloadable: {exc}")

    # 3. Template framing via the real DEFAULT_CHAT_TEMPLATE string
    # (parsed from the module source, never hand-transcribed) for a
    # text-only turn, plus the audio expansion through the processor.
    try:
        import ast

        import jinja2

        source = open(os.path.join(REF_DIR, "processing_lfm3_audio.py")).read()
        template_src = next(
            node.value for node in ast.walk(ast.parse(source)) if isinstance(node, ast.Assign)
            for target in node.targets
            if isinstance(target, ast.Name) and target.id == "DEFAULT_CHAT_TEMPLATE"
        )
        template_text = ast.literal_eval(template_src)
        env = jinja2.Environment()
        template = env.from_string(template_text)
        framed = template.render(
            messages=[{"role": "user", "content": "hello"}],
            bos_token="",
            add_generation_prompt=False,
        )
        result["framed_text_turn"] = framed
    except Exception as exc:  # pragma: no cover - env dependent
        note("template", f"unrenderable: {exc}")

    # 4. End-to-end audio expansion through the real processor
    # (placeholder count + replacement text + matching ids).
    try:
        from lfmpkg.processing_lfm3_audio import Lfm3AudioProcessor

        tokenizer = AutoTokenizer.from_pretrained(REF_DIR, local_files_only=True, trust_remote_code=True)
        processor = Lfm3AudioProcessor(
            feature_extractor=extractor, tokenizer=tokenizer, chat_template=None
        )
        waveform = sine(16000, 440.0, 16000)
        text = "transcribe <|audio|> please"
        out = processor(text=text, audio=[waveform])
        ids = out["input_ids"]
        if hasattr(ids, "tolist"):
            ids = ids.tolist()
        flat = [x for row in (ids if ids and isinstance(ids[0], list) else [ids]) for x in row]
        audio_id = processor.audio_token_id
        count = sum(1 for x in flat if x == audio_id)
        # The reference's own replacement string (audio token x N),
        # framed the way the chat template frames a user turn.
        replacement = processor.replace_audio_token(
            {"num_audio_tokens": [count]}, audio_idx=0
        )
        expanded = text.replace(processor.audio_token, replacement)
        framed = f"<|im_start|>user\n{expanded}<|im_end|>\n"
        result["audio_expansion"] = {
            "text": text,
            "expanded_text": expanded,
            "framed_turn": framed,
            "mel_frames": int(np.asarray(out["feature_attention_mask"]).sum()),
            "encoder_tokens": count,
            "audio_token_id": audio_id,
            "placeholder_runs": text.count("<|audio|>"),
        }
    except Exception as exc:  # pragma: no cover - env dependent
        note("audio_expansion", f"unrunnable: {exc}")

    # 5. Codec frame counts (needs the audio-tokenizer weights).
    try:
        from lfmpkg.processing_lfm3_audio import Lfm3AudioProcessor

        processor = Lfm3AudioProcessor.from_pretrained(
            REF_DIR, local_files_only=True, trust_remote_code=True
        )
        clip = sine(24000, 220.0, 24000)
        codes = processor.encode_audio(clip)
        frames = int(codes.shape[0])
        out_text = "answer <|audio_out|> now"
        out_expanded = out_text.replace(
            processor.audio_out_token, processor.audio_out_token * (frames + 1)
        )
        result["audio_out"] = {
            "samples": 24000,
            "codec_frames": frames,
            "placeholders": frames + 1,
            "eos_code": processor.audio_eos_code,
            "audio_out_token_id": processor.audio_out_token_id,
            "text": out_text,
            "expanded_text": out_expanded,
            "framed_turn": f"<|im_start|>assistant\n{out_expanded}<|im_end|>\n",
        }
    except Exception as exc:  # pragma: no cover - env dependent
        note("audio_out", f"codec unloadable: {exc}")

    print(json.dumps(result, indent=1))


if __name__ == "__main__":
    main()
