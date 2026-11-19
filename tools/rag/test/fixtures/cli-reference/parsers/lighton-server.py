import argparse
import base64
import io
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

MODEL_ID = "lightonai/LightOnOCR-3-1B"
MODEL_REVISION = "23d544085c886bbdace647bcfab91203299417e2"


def select_device(torch, mode):
    if mode == "cpu":
        device = torch.device("cpu")
        dtype = torch.bfloat16
        name = "CPU"
        synchronize = lambda: None
    elif torch.cuda.is_available():
        device = torch.device("cuda:0")
        dtype = torch.bfloat16 if torch.cuda.is_bf16_supported(including_emulation=False) else torch.float32
        name = torch.cuda.get_device_name(device)
        synchronize = torch.cuda.synchronize
    elif torch.backends.mps.is_available():
        device = torch.device("mps")
        dtype = torch.float32
        name = "Apple Metal"
        synchronize = torch.mps.synchronize
    else:
        raise RuntimeError("GPU acceleration is unavailable in this PyTorch environment. Select a GPU-enabled environment or pass --device cpu.")
    info = {"id": MODEL_ID, "device": mode, "deviceName": name,
            "runtime": "ROCm" if device.type == "cuda" and torch.version.hip else device.type,
            "dtype": str(dtype).removeprefix("torch.")}
    return device, dtype, synchronize, info


def serve(args):
    import torch
    from huggingface_hub import snapshot_download
    from PIL import Image
    from safetensors.torch import load_file
    from transformers import LightOnOcrConfig, LightOnOcrForConditionalGeneration, LightOnOcrProcessor

    torch.set_num_threads(min(torch.get_num_threads(), 8))
    device, dtype, synchronize, info = select_device(torch, args.device)
    if args.probe:
        matrix = torch.ones((64, 64), device=device, dtype=dtype)
        product = matrix @ matrix
        synchronize()
        if product[0, 0].item() != 64:
            raise RuntimeError("GPU matrix multiplication check failed")
        print(json.dumps(info), flush=True)
        return

    model_dir = args.model_dir or Path(snapshot_download(
        MODEL_ID, revision=MODEL_REVISION, allow_patterns=["*.json", "*.jinja", "model.safetensors"],
    ))
    processor = LightOnOcrProcessor.from_pretrained(model_dir, local_files_only=True)
    config = LightOnOcrConfig.from_pretrained(model_dir, local_files_only=True)
    previous_dtype = torch.get_default_dtype()
    torch.set_default_dtype(torch.bfloat16)
    try:
        model = LightOnOcrForConditionalGeneration(config).eval()
    finally:
        torch.set_default_dtype(previous_dtype)
    # The pinned checkpoint stores Qwen layers under the older language-model prefix.
    weights = {
        key.replace("language_model.model.", "model.language_model.", 1) if key.startswith("language_model.model.") else key: value
        for key, value in load_file(model_dir / "model.safetensors").items()
    }
    weights.setdefault("lm_head.weight", weights["model.language_model.embed_tokens.weight"])
    model.load_state_dict(weights, strict=True, assign=True)
    del weights
    model.tie_weights()
    model.to(device=device, dtype=dtype)

    class Handler(BaseHTTPRequestHandler):
        def send_json(self, status, data):
            body = json.dumps(data, ensure_ascii=False).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def local_host(self):
            port = self.server.server_port
            return self.headers.get("Host") in (f"127.0.0.1:{port}", f"localhost:{port}")

        def do_GET(self):
            if not self.local_host():
                self.send_json(403, {"error": "Invalid local host"})
            elif self.path == "/v1/models":
                self.send_json(200, {"data": [info]})
            else:
                self.send_json(404, {"error": "Unknown endpoint"})

        def do_POST(self):
            if not self.local_host():
                self.send_json(403, {"error": "Invalid local host"})
                return
            if self.path != "/v1/chat/completions":
                self.send_json(404, {"error": "Unknown endpoint"})
                return
            try:
                length = int(self.headers.get("Content-Length", "0"))
                if self.headers.get_content_type() != "application/json" or not 0 < length <= 16 * 1024**2:
                    raise ValueError("Expected a bounded JSON request")
                request = json.loads(self.rfile.read(length))
                parts = request["messages"][0]["content"]
                prefix = "data:image/png;base64,"
                image_url = parts[0]["image_url"]["url"]
                if request["model"] != MODEL_ID or parts[1]["text"] != "grounding" or not image_url.startswith(prefix):
                    raise ValueError("Expected a PNG image and the LightOnOCR grounding prompt")
                image = Image.open(io.BytesIO(base64.b64decode(image_url[len(prefix):], validate=True)))
                if image.format != "PNG" or image.width * image.height > 1540**2:
                    raise ValueError("Expected a PNG page with at most 1540 × 1540 pixels")
                image = image.convert("RGB")
                max_tokens = int(request.get("max_tokens", 4096))
                if not 0 < max_tokens <= 4096:
                    raise ValueError("Expected max_tokens from 1 to 4096")
            except (AttributeError, IndexError, KeyError, OSError, TypeError, ValueError) as error:
                self.send_json(400, {"error": str(error)})
                return

            try:
                messages = [{"role": "user", "content": [{"type": "image", "image": image}, {"type": "text", "text": "grounding"}]}]
                inputs = processor.apply_chat_template(
                    messages, add_generation_prompt=True, tokenize=True, return_dict=True, return_tensors="pt",
                )
                inputs = {key: value.to(device=device, dtype=dtype) if torch.is_floating_point(value) else value.to(device)
                          for key, value in inputs.items()}
                torch.manual_seed(0)
                synchronize()
                start = time.perf_counter()
                with torch.inference_mode():
                    generated = model.generate(**inputs, max_new_tokens=max_tokens, temperature=0.1, do_sample=True)
                synchronize()
                seconds = time.perf_counter() - start
                tokens = generated[0, inputs["input_ids"].shape[1]:].cpu()
                text = processor.decode(tokens, skip_special_tokens=True)
                print(json.dumps({"inferenceSeconds": round(seconds, 3), "tokens": len(tokens)}, ensure_ascii=True), flush=True)
                self.send_json(200, {"choices": [{"message": {"content": text}, "finish_reason": "length" if len(tokens) == max_tokens else "stop"}]})
            except (RuntimeError, ValueError) as error:
                print(f"LightOnOCR inference failed: {error}", file=sys.stderr, flush=True)
                self.send_json(500, {"error": "LightOnOCR inference failed"})

    with HTTPServer(("127.0.0.1", args.port), Handler) as server:
        print(json.dumps({**info, "url": f"http://127.0.0.1:{server.server_port}/v1"}), flush=True)
        server.serve_forever()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--device", choices=["gpu", "cpu"], default="gpu")
    parser.add_argument("--model-dir", type=Path)
    parser.add_argument("--port", type=int, default=8010)
    parser.add_argument("--probe", action="store_true")
    parser.add_argument("--parent-stdin", action="store_true")
    args = parser.parse_args()
    if args.parent_stdin:
        # Reserve the pipe for lifetime tracking. Libraries can still inspect standard input.
        parent_pipe = os.dup(sys.stdin.fileno())
        with open(os.devnull, "rb") as empty_input:
            os.dup2(empty_input.fileno(), sys.stdin.fileno())

        def exit_with_parent():
            try:
                os.read(parent_pipe, 1)
            finally:
                os._exit(0)

        threading.Thread(target=exit_with_parent, daemon=True).start()
    try:
        serve(args)
    except ImportError as error:
        sys.exit(f"Install PyTorch, torchvision, Pillow and transformers==5.16.1 in the selected Python environment: {error}")
    except RuntimeError as error:
        sys.exit(str(error))
