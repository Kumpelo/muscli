#!/usr/bin/env python3
"""Observe normal playback without changing audio settings or injecting delays."""
import argparse
import datetime as dt
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import threading
import time
import tomllib
from zoneinfo import ZoneInfo


def now_ms():
    return time.time_ns() // 1_000_000


def append(path, value):
    with path.open('a', encoding='utf-8') as file:
        file.write(json.dumps(value, ensure_ascii=False) + '\n')


def command(args, cwd=None):
    result = subprocess.run(args, cwd=cwd, text=True, capture_output=True, timeout=15)
    return {'returncode': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr}


def hardware():
    result = {}
    for card in Path('/proc/asound').glob('card[0-9]*'):
        streams = list(card.glob('stream*'))
        descriptions = []
        for stream in streams:
            try:
                descriptions.append(stream.read_text(errors='replace'))
            except OSError:
                pass
        if not any('HyperX Cloud III' in value for value in descriptions):
            continue
        for path in [*streams, *card.glob('pcm*p/sub*/hw_params'), *card.glob('pcm*p/sub*/status')]:
            try:
                result[str(path)] = path.read_text(errors='replace')
            except OSError as error:
                result[str(path)] = str(error)
        device = Path('/sys/class/sound') / card.name / 'device'
        result[str(device)] = str(device.resolve())
    return result


def analyze(directory, quiet=False):
    manifest = json.loads((directory / 'manifest.json').read_text())
    rows = []
    malformed = 0
    audio = directory / 'audio.jsonl'
    if audio.exists():
        for line in audio.read_text().splitlines():
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                malformed += 1
    cutoff = manifest.get('ended_unix_ms', now_ms())
    rows = [row for row in rows if row.get('unix_ms', 0) <= cutoff]
    faults = [row for row in rows if row.get('event') == 'device_fault']
    ring = [row for row in rows if row.get('event') == 'ring_starved']
    opens = [row for row in rows if row.get('event') == 'stream_open']
    negotiated = [row for row in rows if row.get('event') == 'stream_negotiated']
    snapshots = [row for row in rows if row.get('event') in ('ring_snapshot', 'stream_closed')]
    frames = {}
    rates = {(row.get('pid'), row['stream']): row['sample_rate'] for row in opens}
    for row in snapshots:
        key = (row.get('pid'), row['stream'])
        frames[key] = max(frames.get(key, 0), row.get('delivered_frames', 0))
    delivered_seconds = sum(value / rates[key] for key, value in frames.items() if key in rates)
    coverage_gaps = []
    for start in (row for row in rows if row.get('event') == 'stream_started'):
        key = (start.get('pid'), start['stream'])
        stream_rows = [row for row in snapshots if (row.get('pid'), row['stream']) == key]
        end = next((row['unix_ms'] for row in stream_rows if row.get('event') == 'stream_closed'), cutoff)
        stamps = sorted({start['unix_ms'], end, *(row['unix_ms'] for row in stream_rows)})
        coverage_gaps.extend({'pid': key[0], 'stream': key[1], 'from_unix_ms': a, 'to_unix_ms': b}
                             for a, b in zip(stamps, stamps[1:]) if b - a > 3000)
    usb_events = []
    kernel = directory / 'kernel-journal.jsonl'
    if kernel.exists():
        for line in kernel.read_text().splitlines():
            try:
                row = json.loads(line)
                if re.search(r'USB disconnect|reset .*USB device|New USB device|Product: HyperX|snd_usb_audio', row.get('line', '')):
                    usb_events.append(row)
            except json.JSONDecodeError:
                pass
    report = {
        'conclusion': 'pendiente de reproducción y revisión auditiva',
        'elapsed_minutes': round((cutoff - manifest['started_unix_ms']) / 60000, 2),
        'delivered_audio_minutes_approx': round(delivered_seconds / 60, 2),
        'device_xrun_events': sum(row.get('kind') == 'Xrun' for row in faults),
        'other_device_faults': [row for row in faults if row.get('kind') != 'Xrun'],
        'ring_starvation_events_inside_track': ring,
        'stream_open_count': len(opens),
        'formats': opens,
        'negotiated_periods': negotiated,
        'pipeline_formats': [row for row in rows if row.get('event') == 'pipeline_format'],
        'native_handovers': sum(row.get('event') == 'native_handover' for row in rows),
        'diagnostics_drops': [row for row in rows if row.get('event') == 'diagnostics_dropped'],
        'engine_errors': [row for row in rows if row.get('event') == 'engine_error'],
        'listening_markers': [json.loads(line) for line in (directory / 'markers.jsonl').read_text().splitlines()] if (directory / 'markers.jsonl').exists() else [],
        'malformed_lines': malformed,
        'snapshot_gaps_over_3s': coverage_gaps,
        'usb_events_to_review_not_automatically_attributed': usb_events,
        'warning': 'Elapsed time and delivered frames do not prove normal music or acceptable latency. Review listening markers, journal, PipeWire changes and logging coverage. Raw missing-frame counters can include normal end-of-file silence.',
    }
    (directory / 'summary.json').write_text(json.dumps(report, indent=2, ensure_ascii=False) + '\n')
    if not quiet:
        print(json.dumps({key: report[key] for key in ('conclusion', 'elapsed_minutes', 'delivered_audio_minutes_approx', 'device_xrun_events', 'stream_open_count')}, indent=2, ensure_ascii=False))
    return report


def collect(args):
    repo = args.repo.resolve()
    binary = repo / 'target/release/muscli'
    if not binary.exists():
        raise SystemExit('Compila primero muscli con cargo build --release --locked.')
    running = []
    for path in Path('/proc').glob('[0-9]*/comm'):
        try:
            if path.read_text().strip() == 'muscli':
                running.append(path.parent.name)
        except OSError:
            pass
    if running:
        raise SystemExit(f'Cierra la sesión muscli actual antes de iniciar el binario instrumentado. PID: {", ".join(running)}. No se ha cerrado ni modificado.')
    default_sink = command(['wpctl', 'inspect', '@DEFAULT_AUDIO_SINK@'])
    if default_sink['returncode'] != 0 or 'HyperX' not in default_sink['stdout']:
        raise SystemExit('La salida predeterminada no se identifica como HyperX. No se cambió la ruta; compruébala antes de esta prueba.')
    directory = args.output.resolve()
    directory.mkdir(parents=True, exist_ok=False, mode=0o700)
    config_path = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home() / '.config'))) / 'muscli/config.toml'
    config = tomllib.loads(config_path.read_text()) if config_path.exists() else {}
    manifest = {
        'started_unix_ms': now_ms(), 'planned_minutes': args.minutes,
        'local_start': dt.datetime.now(ZoneInfo('America/Santiago')).isoformat(),
        'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'head': command(['git', 'rev-parse', 'HEAD'], repo),
        'worktree': command(['git', 'status', '--short', '--branch'], repo),
        'config': {key: config.get(key) for key in ('audio_backend', 'audio_device', 'gapless', 'replaygain_enabled', 'replaygain_mode', 'bit_perfect', 'mix_crossfade_seconds', 'equalizer')},
        'route': command(['wpctl', 'status', '-n']),
        'default_sink': default_sink,
        'initial_hyperx_hardware': hardware(),
        'injected_delays': False,
    }
    (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')
    (directory / 'markers.jsonl').touch()
    monitors, pumps = [], []
    commands = {
        'pipewire-journal': ['journalctl', '--user', '-f', '--since', 'now', '-o', 'short-unix', '-u', 'pipewire', '-u', 'pipewire-pulse', '-u', 'wireplumber'],
        'kernel-journal': ['journalctl', '-k', '-f', '--since', 'now', '-o', 'short-unix'],
        'pipewire-graph': ['pw-dump', '-m'],
        'pipewire-counters': ['pw-top', '-b'],
    }
    def pump(process, path):
        for line in process.stdout:
            append(path, {'unix_ms': now_ms(), 'line': line.rstrip('\n')})
    for name, cmd in commands.items():
        try:
            process = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            thread = threading.Thread(target=pump, args=(process, directory / f'{name}.jsonl'), daemon=True)
            thread.start()
            monitors.append((name, process))
            pumps.append(thread)
        except OSError as error:
            append(directory / 'collector-errors.jsonl', {'unix_ms': now_ms(), 'source': name, 'error': str(error)})
    env = dict(os.environ, MUSCLI_AUDIO_LOG=str(directory / 'audio.jsonl'))
    finished = threading.Event()
    timer_done = threading.Event()
    def watch():
        deadline = time.monotonic() + args.minutes * 60
        try:
            while time.monotonic() < deadline:
                append(directory / 'hardware.jsonl', {'unix_ms': now_ms(), 'hardware': hardware()})
                if finished.wait(1):
                    break
        finally:
            for name, process in monitors:
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                append(directory / 'monitor-exits.jsonl', {'source': name, 'returncode': process.returncode})
            for thread in pumps:
                thread.join(timeout=2)
            manifest['ended_unix_ms'] = now_ms()
            manifest['final_route'] = command(['wpctl', 'status', '-n'])
            (directory / 'manifest.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + '\n')
            analyze(directory, quiet=True)
            timer_done.set()
    watcher = threading.Thread(target=watch, daemon=True)
    watcher.start()
    print(f'Captura de {args.minutes} minutos: {directory}\nReproduce música normalmente. No se cambia ninguna preferencia.\nAl terminar el plazo se cierra la captura externa, no el reproductor.', flush=True)
    try:
        player = subprocess.Popen([str(binary)], env=env, cwd=repo)
        manifest['player_pid'] = player.pid
        manifest['player_returncode'] = player.wait()
    finally:
        finished.set()
        if not timer_done.wait(timeout=45):
            print("La captura no terminó de cerrar; revisar sus logs antes de sacar conclusiones.")
        else:
            print(f"Evidencia y resumen: {directory / 'summary.json'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    run = sub.add_parser('start')
    run.add_argument('--minutes', type=int, choices=range(30, 61), default=45)
    run.add_argument('--repo', type=Path, default=Path(__file__).resolve().parents[1])
    run.add_argument('--output', type=Path, required=True)
    mark = sub.add_parser('mark')
    mark.add_argument('directory', type=Path)
    mark.add_argument('note')
    read = sub.add_parser('analyze')
    read.add_argument('directory', type=Path)
    args = parser.parse_args()
    if args.action == 'start':
        collect(args)
    elif args.action == 'mark':
        append(args.directory / 'markers.jsonl', {'unix_ms': now_ms(), 'local': dt.datetime.now(ZoneInfo('America/Santiago')).isoformat(), 'note': args.note})
    else:
        analyze(args.directory)


if __name__ == '__main__':
    main()
