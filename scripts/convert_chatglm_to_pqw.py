#!/usr/bin/env python3
"""
ChatGLM-3-6B to POLER .pqw Streaming Converter (Pure Python + NumPy).

Streams safetensors shards directly from HuggingFace (or local dir),
quantizes weights to INT4 with per-row scales, extracts ChatGLM3 SentencePiece
tokenizer with byte-fallback, and produces a sovereign zero-dependency .pqw file.
"""

import sys
import os
import json
import struct
import hashlib
import urllib.request
import numpy as np

PAGE = 4096
MAGIC = b"POLERQW\x00"
VERSION = 2
HEADER_SIZE = 128

MODEL_TYPE_DECODER = 1
QUANT_INT4 = 2

def parse_spm_tokenizer(path):
    with open(path, 'rb') as f:
        data = f.read()
    pos = 0
    pieces = []
    while pos < len(data):
        tag = 0
        shift = 0
        while True:
            b = data[pos]
            pos += 1
            tag |= (b & 0x7F) << shift
            if not (b & 0x80): break
            shift += 7
        fn = tag >> 3
        wt = tag & 7
        if wt == 0:
            v = 0
            shift = 0
            while True:
                b = data[pos]
                pos += 1
                v |= (b & 0x7F) << shift
                if not (b & 0x80): break
                shift += 7
        elif wt == 1: pos += 8
        elif wt == 2:
            length = 0
            shift = 0
            while True:
                b = data[pos]
                pos += 1
                length |= (b & 0x7F) << shift
                if not (b & 0x80): break
                shift += 7
            field_data = data[pos:pos+length]
            pos += length
            if fn == 1:
                ppos = 0
                ptext = b''
                pscore = 0.0
                ptype = 1
                while ppos < len(field_data):
                    ptag = 0
                    pshift = 0
                    while True:
                        pb = field_data[ppos]
                        ppos += 1
                        ptag |= (pb & 0x7F) << pshift
                        if not (pb & 0x80): break
                        pshift += 7
                    pfn = ptag >> 3
                    pwt = ptag & 7
                    if pwt == 0:
                        pv = 0
                        pshift = 0
                        while True:
                            pb = field_data[ppos]
                            ppos += 1
                            pv |= (pb & 0x7F) << pshift
                            if not (pb & 0x80): break
                            pshift += 7
                        if pfn == 3: ptype = pv
                    elif pwt == 1: ppos += 8
                    elif pwt == 2:
                        plen = 0
                        pshift = 0
                        while True:
                            pb = field_data[ppos]
                            ppos += 1
                            plen |= (pb & 0x7F) << pshift
                            if not (pb & 0x80): break
                            pshift += 7
                        ptext = field_data[ppos:ppos+plen]
                        ppos += plen
                    elif pwt == 5:
                        pscore = struct.unpack('<f', field_data[ppos:ppos+4])[0]
                        ppos += 4
                pieces.append((ptext, pscore, ptype))
        elif wt == 5: pos += 4
    return pieces

def build_tokenizer_section(pieces):
    out = bytearray(b"TOKR")
    out.extend(struct.pack("<H", 2))
    out.append(0)
    out.append(1)
    out.extend(struct.pack("<IIIII", 0, 1, 2, 3, 64789))
    out.extend(struct.pack("<I", len(pieces)))
    for ptext, pscore, _ in pieces:
        out.extend(struct.pack("<H", len(ptext)))
        out.extend(ptext)
        out.extend(struct.pack("<f", pscore))
    specials = [
        (b"[MASK]", 64789),
        (b"[gMASK]", 64790),
        (b"[sMASK]", 64791),
        (b"sop", 64792),
        (b"eop", 64793),
        (b"<|system|>", 64794),
        (b"<|user|>", 64795),
        (b"<|assistant|>", 64796),
        (b"<|observation|>", 64797),
    ]
    out.extend(struct.pack("<I", len(specials)))
    for stext, sid in specials:
        out.extend(struct.pack("<H", len(stext)))
        out.extend(stext)
        out.extend(struct.pack("<I", sid))
    out.append(2)
    return bytes(out)

def quantize_int4(w_f32):
    rows, cols = w_f32.shape
    scales = np.max(np.abs(w_f32), axis=1) / 7.0
    scales[scales == 0] = 1.0
    q = np.clip(np.round(w_f32 / scales[:, None]), -7, 7).astype(np.int8)
    if cols % 2 != 0:
        q = np.pad(q, ((0, 0), (0, 1)), mode='constant')
        cols += 1
    nib_lo = (q[:, 0::2] + 8).astype(np.uint8)
    nib_hi = (q[:, 1::2] + 8).astype(np.uint8)
    packed = ((nib_hi << 4) | nib_lo).tobytes()
    scale_bytes = scales.astype('<f4').tobytes()
    return packed, scale_bytes, list(scales)

def parse_safetensors_metadata(file_path):
    with open(file_path, 'rb') as f:
        header_len_bytes = f.read(8)
        if len(header_len_bytes) < 8:
            return None, 0
        header_len = struct.unpack('<Q', header_len_bytes)[0]
        header_json = f.read(header_len).decode('utf-8')
        metadata = json.loads(header_json)
        return metadata, 8 + header_len

def load_safetensors_tensor(f, offset_base, tensor_info):
    dtype_str = tensor_info['dtype']
    shape = tensor_info['shape']
    data_offsets = tensor_info['data_offsets']
    f.seek(offset_base + data_offsets[0])
    raw_data = f.read(data_offsets[1] - data_offsets[0])
    if dtype_str == 'F16':
        arr = np.frombuffer(raw_data, dtype=np.float16).astype(np.float32)
    elif dtype_str == 'BF16':
        u16 = np.frombuffer(raw_data, dtype=np.uint16)
        u32 = u16.astype(np.uint32) << 16
        arr = u32.view(np.float32)
    elif dtype_str == 'F32':
        arr = np.frombuffer(raw_data, dtype=np.float32)
    else:
        raise ValueError(f"Unsupported dtype: {dtype_str}")
    return arr.reshape(shape)

class StreamingPqwWriter:
    def __init__(self, out_path, layers=28, hidden=4096, heads=32, intermediate=13696, vocab=65024, max_pos=8192, kv_heads=2):
        self.out_path = out_path
        self.f = open(out_path, 'wb')
        self.f.seek(PAGE)
        self.layers = layers
        self.hidden = hidden
        self.heads = heads
        self.intermediate = intermediate
        self.vocab = vocab
        self.max_pos = max_pos
        self.kv_heads = kv_heads
        self.entries = []
        self.hasher = hashlib.sha256()

    def add_tensor(self, name, dtype, dims, scales, data_bytes):
        curr_pos = self.f.tell()
        aligned = (curr_pos + PAGE - 1) // PAGE * PAGE
        if aligned > curr_pos:
            pad = b'\x00' * (aligned - curr_pos)
            self.f.write(pad)
            self.hasher.update(pad)
        data_off = aligned
        self.f.write(data_bytes)
        self.hasher.update(data_bytes)
        self.entries.append({
            'name': name,
            'dtype': dtype,
            'dims': dims,
            'scales': scales,
            'data_off': data_off,
            'data_len': len(data_bytes)
        })

    def finish(self):
        curr_pos = self.f.tell()
        table_offset = (curr_pos + PAGE - 1) // PAGE * PAGE
        if table_offset > curr_pos:
            pad = b'\x00' * (table_offset - curr_pos)
            self.f.write(pad)
            self.hasher.update(pad)
        table_start = self.f.tell()
        table_buf = bytearray()
        for t in self.entries:
            name_b = t['name'].encode('utf-8')
            table_buf.extend(struct.pack('<H', len(name_b)))
            table_buf.extend(name_b)
            table_buf.append(t['dtype'])
            table_buf.append(len(t['dims']))
            table_buf.extend(struct.pack('<H', 0))
            table_buf.extend(struct.pack('<I', len(t['scales'])))
            for d in t['dims']:
                table_buf.extend(struct.pack('<Q', d))
            table_buf.extend(struct.pack('<Q', t['data_off']))
            table_buf.extend(struct.pack('<Q', t['data_len']))
            for s in t['scales']:
                table_buf.extend(struct.pack('<f', s))
        self.f.write(table_buf)
        self.hasher.update(table_buf)
        table_len = len(table_buf)
        file_len = self.f.tell()
        digest = self.hasher.digest()
        head = bytearray(HEADER_SIZE)
        head[0:8] = MAGIC
        struct.pack_into('<I', head, 8, VERSION)
        struct.pack_into('<I', head, 12, HEADER_SIZE)
        head[16] = MODEL_TYPE_DECODER
        head[17] = QUANT_INT4
        struct.pack_into('<H', head, 18, 0)
        struct.pack_into('<I', head, 20, self.layers)
        struct.pack_into('<I', head, 24, self.hidden)
        struct.pack_into('<I', head, 28, self.intermediate)
        struct.pack_into('<I', head, 32, self.heads)
        head_dim = self.hidden // self.heads
        struct.pack_into('<I', head, 36, head_dim)
        struct.pack_into('<I', head, 40, self.vocab)
        struct.pack_into('<I', head, 44, self.max_pos)
        struct.pack_into('<I', head, 48, 0)
        struct.pack_into('<I', head, 52, 0)
        struct.pack_into('<I', head, 56, self.kv_heads)
        struct.pack_into('<I', head, 60, 0)
        struct.pack_into('<Q', head, 64, table_offset)
        struct.pack_into('<Q', head, 72, table_len)
        struct.pack_into('<Q', head, 80, table_offset - PAGE)
        struct.pack_into('<Q', head, 88, file_len)
        head[96:128] = digest
        self.f.seek(0)
        self.f.write(head)
        self.f.close()
        print(f"\n[PQW] Successfully wrote {file_len / (1024*1024):.2f} MB to {self.out_path}")

def download_file(url, target_path):
    print(f"[Download] Downloading {url} -> {target_path} ...")
    req = urllib.request.Request(
        url, 
        headers={'User-Agent': 'Mozilla/5.0 (POLER-Engine Sovereign Downloader)'}
    )
    with urllib.request.urlopen(req) as response, open(target_path, 'wb') as out_file:
        length = int(response.headers.get('Content-Length', 0))
        downloaded = 0
        chunk_size = 1024 * 1024
        while True:
            chunk = response.read(chunk_size)
            if not chunk:
                break
            downloaded += len(chunk)
            out_file.write(chunk)
            if length > 0:
                percent = downloaded / length * 100
                sys.stdout.write(f"\r  Progress: {percent:.1f}% ({downloaded / (1024*1024):.1f} MB / {length / (1024*1024):.1f} MB)")
                sys.stdout.flush()
    print("")

def convert_chatglm3(model_dir_or_repo, out_pqw_path):
    is_hf_repo = "/" in model_dir_or_repo and not os.path.exists(model_dir_or_repo)
    base_url = f"https://huggingface.co/{model_dir_or_repo}/resolve/main" if is_hf_repo else ""
    tok_model_path = "/tmp/chatglm3_tokenizer.model"
    if not os.path.exists(tok_model_path):
        if is_hf_repo:
            download_file(f"{base_url}/tokenizer.model", tok_model_path)
        else:
            tok_model_path = os.path.join(model_dir_or_repo, "tokenizer.model")
    print("[Tokenizer] Parsing SPM tokenizer...")
    spm_pieces = parse_spm_tokenizer(tok_model_path)
    tok_section_bytes = build_tokenizer_section(spm_pieces)
    print(f"[Tokenizer] Built section ({len(tok_section_bytes)} bytes, {len(spm_pieces)} pieces)")
    index_path = "/tmp/model.safetensors.index.json"
    if is_hf_repo:
        if not os.path.exists(index_path):
            download_file(f"{base_url}/model.safetensors.index.json", index_path)
        with open(index_path, 'r') as f:
            index_data = json.load(f)
    else:
        with open(os.path.join(model_dir_or_repo, "model.safetensors.index.json"), 'r') as f:
            index_data = json.load(f)
    weight_map = index_data.get('weight_map', {})
    shards = sorted(list(set(weight_map.values())))
    print(f"[Model] Found {len(shards)} safetensors shards.")
    writer = StreamingPqwWriter(out_pqw_path, layers=28, hidden=4096, heads=32, intermediate=13696, vocab=65024, max_pos=8192, kv_heads=2)
    writer.add_tensor("__tokenizer__", 3, [len(tok_section_bytes)], [], tok_section_bytes)
    for s_idx, shard_name in enumerate(shards):
        print(f"\n[Shard {s_idx+1}/{len(shards)}] Processing {shard_name} ...")
        if is_hf_repo:
            shard_path = f"/tmp/{shard_name}"
            if not os.path.exists(shard_path):
                download_file(f"{base_url}/{shard_name}", shard_path)
        else:
            shard_path = os.path.join(model_dir_or_repo, shard_name)
        meta, base_off = parse_safetensors_metadata(shard_path)
        with open(shard_path, 'rb') as f:
            for k, info in meta.items():
                if k == '__metadata__': continue
                if k == 'transformer.embedding.word_embeddings.weight':
                    print(f"  Quantizing {k} -> word_embeddings ...")
                    arr = load_safetensors_tensor(f, base_off, info)
                    packed, _, scales = quantize_int4(arr)
                    writer.add_tensor("word_embeddings", 2, list(arr.shape), scales, packed)
                elif k == 'transformer.encoder.final_layernorm.weight':
                    print(f"  Converting {k} -> final_norm_gamma (f32) ...")
                    arr = load_safetensors_tensor(f, base_off, info)
                    writer.add_tensor("final_norm_gamma", 0, list(arr.shape), [], arr.astype('<f4').tobytes())
                elif k == 'transformer.output_layer.weight':
                    print(f"  Quantizing {k} -> lm_head_w ...")
                    arr = load_safetensors_tensor(f, base_off, info)
                    packed, _, scales = quantize_int4(arr)
                    writer.add_tensor("lm_head_w", 2, list(arr.shape), scales, packed)
                elif k.startswith('transformer.encoder.layers.'):
                    parts = k.split('.')
                    l_idx = int(parts[3])
                    sub = '.'.join(parts[4:])
                    if sub == 'input_layernorm.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        writer.add_tensor(f"layers.{l_idx}.attn_norm_gamma", 0, list(arr.shape), [], arr.astype('<f4').tobytes())
                    elif sub == 'post_attention_layernorm.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        writer.add_tensor(f"layers.{l_idx}.ffn_norm_gamma", 0, list(arr.shape), [], arr.astype('<f4').tobytes())
                    elif sub == 'self_attention.query_key_value.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        q_w = arr[0:4096, :]
                        k_w = arr[4096:4352, :]
                        v_w = arr[4352:4608, :]
                        p, _, sc = quantize_int4(q_w)
                        writer.add_tensor(f"layers.{l_idx}.q_proj_w", 2, list(q_w.shape), sc, p)
                        p, _, sc = quantize_int4(k_w)
                        writer.add_tensor(f"layers.{l_idx}.k_proj_w", 2, list(k_w.shape), sc, p)
                        p, _, sc = quantize_int4(v_w)
                        writer.add_tensor(f"layers.{l_idx}.v_proj_w", 2, list(v_w.shape), sc, p)
                    elif sub == 'self_attention.dense.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        p, _, sc = quantize_int4(arr)
                        writer.add_tensor(f"layers.{l_idx}.out_proj_w", 2, list(arr.shape), sc, p)
                    elif sub == 'mlp.dense_h_to_4h.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        gate_w = arr[0:13696, :]
                        up_w = arr[13696:27392, :]
                        p, _, sc = quantize_int4(gate_w)
                        writer.add_tensor(f"layers.{l_idx}.ffn_gate_w", 2, list(gate_w.shape), sc, p)
                        p, _, sc = quantize_int4(up_w)
                        writer.add_tensor(f"layers.{l_idx}.ffn_up_w", 2, list(up_w.shape), sc, p)
                    elif sub == 'mlp.dense_4h_to_h.weight':
                        arr = load_safetensors_tensor(f, base_off, info)
                        p, _, sc = quantize_int4(arr)
                        writer.add_tensor(f"layers.{l_idx}.ffn_down_w", 2, list(arr.shape), sc, p)
        if is_hf_repo and os.path.exists(shard_path):
            os.remove(shard_path)
            print(f"  Cleaned up temporary shard {shard_path}")
    writer.finish()

if __name__ == '__main__':
    src = sys.argv[1] if len(sys.argv) > 1 else "THUDM/chatglm3-6b"
    dst = sys.argv[2] if len(sys.argv) > 2 else "models/chatglm3-6b.pqw"
    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    convert_chatglm3(src, dst)
