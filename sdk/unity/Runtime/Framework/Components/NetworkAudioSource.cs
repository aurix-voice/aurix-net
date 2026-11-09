// Aurix Net Unity SDK — replicated AudioSource (settings + playback commands).
// SPDX-License-Identifier: Apache-2.0
using System;
using System.Collections.Generic;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>
    /// Networks an <see cref="AudioSource"/>: volume/pitch/loop/mute/spatial settings replicate as sync vars, Play/Stop/
    /// Pause/UnPause/PlayOneShot are RPCs. Clips travel as indices into <see cref="clips"/> (same list on every peer).
    /// Controller is the owner (<see cref="ownerAuth"/>) or the server.
    /// </summary>
    public class NetworkAudioSource : NetworkBehaviour
    {
        public AudioSource audioSource;
        [Tooltip("Owner controls playback (otherwise the server).")] public bool ownerAuth = true;
        [Tooltip("Clips that may be played over the network, same order on every peer.")] public List<AudioClip> clips = new List<AudioClip>();

        public readonly SyncVar<float> volume = new SyncVar<float>(1f), pitch = new SyncVar<float>(1f), spatialBlend = new SyncVar<float>(0f), minDistance = new SyncVar<float>(1f), maxDistance = new SyncVar<float>(500f);
        public readonly SyncVar<bool> loop = new SyncVar<bool>(false), mute = new SyncVar<bool>(false);
        public readonly SyncVar<int> clipIndex = new SyncVar<int>(-1);
        public readonly SyncVar<bool> playing = new SyncVar<bool>(false);

        public bool isPlaying => playing.value;
        public AudioClip clip => clipIndex.value >= 0 && clipIndex.value < clips.Count ? clips[clipIndex.value] : null;

        protected override void OnBeforeBind()
        {
            foreach (var v in new NetworkModule[] { volume, pitch, spatialBlend, minDistance, maxDistance, loop, mute, clipIndex, playing }) v.SetAuthority(ownerAuth);
            volume.onChanged += v => { if (audioSource != null) audioSource.volume = v; };
            pitch.onChanged += v => { if (audioSource != null) audioSource.pitch = v; };
            spatialBlend.onChanged += v => { if (audioSource != null) audioSource.spatialBlend = v; };
            minDistance.onChanged += v => { if (audioSource != null) audioSource.minDistance = v; };
            maxDistance.onChanged += v => { if (audioSource != null) audioSource.maxDistance = v; };
            loop.onChanged += v => { if (audioSource != null) audioSource.loop = v; };
            mute.onChanged += v => { if (audioSource != null) audioSource.mute = v; };
            clipIndex.onChanged += _ => { if (audioSource != null) audioSource.clip = clip; };
            playing.onChanged += v => { if (audioSource == null || isController) return; if (v && !audioSource.isPlaying) audioSource.Play(); else if (!v && audioSource.isPlaying) audioSource.Stop(); };
        }

        protected override void OnSpawned(bool asServer)
        {
            if (audioSource == null) return;
            audioSource.volume = volume.value; audioSource.pitch = pitch.value; audioSource.spatialBlend = spatialBlend.value; audioSource.minDistance = minDistance.value; audioSource.maxDistance = maxDistance.value;
            audioSource.loop = loop.value; audioSource.mute = mute.value; audioSource.clip = clip;
            if (!isController) { if (playing.value && !audioSource.isPlaying) audioSource.Play(); else if (!playing.value && audioSource.isPlaying) audioSource.Stop(); }
        }

        /// <summary>Controller: mirror the AudioSource's current settings into the sync vars (call after editing it directly).</summary>
        public void PushSettings()
        {
            if (!isController || audioSource == null) return;
            volume.value = audioSource.volume; pitch.value = audioSource.pitch; spatialBlend.value = audioSource.spatialBlend; minDistance.value = audioSource.minDistance; maxDistance.value = audioSource.maxDistance;
            loop.value = audioSource.loop; mute.value = audioSource.mute;
        }

        public void Play() { if (!isController) return; if (audioSource != null) audioSource.Play(); playing.value = true; }
        public void Play(int clipId) { if (!isController) return; clipIndex.value = clipId; if (audioSource != null) audioSource.clip = clip; Play(); }
        public void Play(AudioClip c) { int i = clips.IndexOf(c); if (i < 0) { Debug.LogError("NetworkAudioSource: clip is not in the clips list"); return; } Play(i); }
        public void Stop() { if (!isController) return; if (audioSource != null) audioSource.Stop(); playing.value = false; }
        public void Pause() { if (!isController) return; if (audioSource != null) audioSource.Pause(); Rpc(nameof(RpcPause)); }
        public void UnPause() { if (!isController) return; if (audioSource != null) audioSource.UnPause(); Rpc(nameof(RpcUnPause)); }
        /// <summary>Controller: play a one-shot on every peer (fire-and-forget, not replayed to late joiners).</summary>
        public void PlayOneShot(int clipId, float volumeScale = 1f) { if (!isController) return; if (isServer) Rpc(nameof(RpcOneShot), clipId, volumeScale); else Rpc(nameof(CmdOneShot), clipId, volumeScale); }
        public void PlayOneShot(AudioClip c, float volumeScale = 1f) { int i = clips.IndexOf(c); if (i >= 0) PlayOneShot(i, volumeScale); }

        void DoOneShot(int clipId, float scale) { if (audioSource != null && clipId >= 0 && clipId < clips.Count) audioSource.PlayOneShot(clips[clipId], scale); }
        [ServerRpc(requireOwnership: true)] void CmdOneShot(int clipId, float scale) { Rpc(nameof(RpcOneShot), clipId, scale); }
        [ObserversRpc(runLocally: true)] void RpcOneShot(int clipId, float scale) => DoOneShot(clipId, scale);
        [ObserversRpc] void RpcPause() { if (!isController && audioSource != null) audioSource.Pause(); }
        [ObserversRpc] void RpcUnPause() { if (!isController && audioSource != null) audioSource.UnPause(); }
    }
}
