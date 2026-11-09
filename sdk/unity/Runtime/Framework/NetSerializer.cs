// Bit-level serializer for the gameplay framework: primitives, Unity math types, strings, byte
// arrays, enums, arrays/lists of those, PlayerID, NetworkIdentity references and INetSerializable.
using System;
using System.Collections;
using System.Collections.Generic;
using Aurix.Net.Managed;
using UnityEngine;

namespace Aurix.Net.Unity.Framework
{
    /// <summary>A player in the room: the Aurix actor id.</summary>
    public readonly struct PlayerID : IEquatable<PlayerID>
    {
        public readonly ushort id;
        public PlayerID(ushort id) { this.id = id; }
        public bool Equals(PlayerID o) => id == o.id;
        public override bool Equals(object o) => o is PlayerID p && p.id == id;
        public override int GetHashCode() => id;
        public override string ToString() => "P" + id;
        public static bool operator ==(PlayerID a, PlayerID b) => a.id == b.id;
        public static bool operator !=(PlayerID a, PlayerID b) => a.id != b.id;
        public static implicit operator ushort(PlayerID p) => p.id;
    }

    /// <summary>Implement on your own structs/classes to send them through SyncVars and RPCs.</summary>
    public interface INetSerializable
    {
        void Write(BitWriter w);
        void Read(BitReader r);
    }

    public static class NetSerializer
    {
        public const int MaxString = 65535, MaxBytes = 1 << 20, MaxCount = 1 << 16;

        /// <summary>Resolver for NetworkIdentity references; set by the NetworkManager.</summary>
        public static Func<uint, NetworkIdentity> ResolveIdentity;

        public static void Write(BitWriter w, object v, Type t)
        {
            if (t.IsEnum) t = Enum.GetUnderlyingType(t);
            switch (Type.GetTypeCode(t))
            {
                case TypeCode.Boolean: w.WriteBool((bool)v); return;
                case TypeCode.Byte: w.WriteBits((byte)v, 8); return;
                case TypeCode.SByte: w.WriteBits((byte)(sbyte)v, 8); return;
                case TypeCode.Int16: w.WriteBits((ushort)(short)v, 16); return;
                case TypeCode.UInt16: w.WriteBits((ushort)v, 16); return;
                case TypeCode.Int32: w.WriteBits((uint)(int)v, 32); return;
                case TypeCode.UInt32: w.WriteBits((uint)v, 32); return;
                case TypeCode.Int64: w.WriteBits((ulong)(long)v, 64); return;
                case TypeCode.UInt64: w.WriteBits((ulong)v, 64); return;
                case TypeCode.Single: w.WriteF32((float)v); return;
                case TypeCode.Double: w.WriteBits((ulong)BitConverter.DoubleToInt64Bits((double)v), 64); return;
                case TypeCode.Char: w.WriteBits((char)v, 16); return;
                case TypeCode.String: WriteString(w, (string)v); return;
            }
            if (t == typeof(Vector2)) { var x = (Vector2)v; w.WriteF32(x.x); w.WriteF32(x.y); return; }
            if (t == typeof(Vector3)) { var x = (Vector3)v; w.WriteF32(x.x); w.WriteF32(x.y); w.WriteF32(x.z); return; }
            if (t == typeof(Quaternion)) { var q = (Quaternion)v; w.WriteF32(q.x); w.WriteF32(q.y); w.WriteF32(q.z); w.WriteF32(q.w); return; }
            if (t == typeof(Color)) { var c = (Color)v; w.WriteF32(c.r); w.WriteF32(c.g); w.WriteF32(c.b); w.WriteF32(c.a); return; }
            if (t == typeof(PlayerID)) { w.WriteBits(((PlayerID)v).id, 16); return; }
            if (t == typeof(byte[])) { var b = (byte[])v ?? Array.Empty<byte>(); w.WriteUInt((ulong)b.Length, 20); w.WriteBytes(b); return; }
            if (typeof(NetworkIdentity).IsAssignableFrom(t)) { var id = (NetworkIdentity)v; w.WriteBits(id != null && id.isSpawned ? id.objectId : 0u, 32); return; }
            if (typeof(INetSerializable).IsAssignableFrom(t))
            {
                bool has = v != null || t.IsValueType; w.WriteBool(has);
                if (has) ((INetSerializable)v).Write(w);
                return;
            }
            if (t.IsArray)
            {
                var a = (Array)v; var et = t.GetElementType();
                w.WriteUInt((ulong)(a?.Length ?? 0), 16);
                if (a != null) foreach (var e in a) Write(w, e, et);
                return;
            }
            if (t.IsGenericType && t.GetGenericTypeDefinition() == typeof(List<>))
            {
                var l = (IList)v; var et = t.GetGenericArguments()[0];
                w.WriteUInt((ulong)(l?.Count ?? 0), 16);
                if (l != null) foreach (var e in l) Write(w, e, et);
                return;
            }
            if (t.IsGenericType && t.GetGenericTypeDefinition() == typeof(Nullable<>))
            {
                bool has = v != null; w.WriteBool(has);
                if (has) Write(w, v, Nullable.GetUnderlyingType(t));
                return;
            }
            throw new NetException("cannot serialize " + t.FullName + " (implement INetSerializable)");
        }

        public static object Read(BitReader r, Type t)
        {
            var orig = t;
            if (t.IsEnum) t = Enum.GetUnderlyingType(t);
            object o = null;
            switch (Type.GetTypeCode(t))
            {
                case TypeCode.Boolean: o = r.ReadBool(); break;
                case TypeCode.Byte: o = (byte)r.ReadBits(8); break;
                case TypeCode.SByte: o = (sbyte)(byte)r.ReadBits(8); break;
                case TypeCode.Int16: o = (short)(ushort)r.ReadBits(16); break;
                case TypeCode.UInt16: o = (ushort)r.ReadBits(16); break;
                case TypeCode.Int32: o = (int)(uint)r.ReadBits(32); break;
                case TypeCode.UInt32: o = (uint)r.ReadBits(32); break;
                case TypeCode.Int64: o = (long)r.ReadBits(64); break;
                case TypeCode.UInt64: o = r.ReadBits(64); break;
                case TypeCode.Single: o = r.ReadF32(); break;
                case TypeCode.Double: o = BitConverter.Int64BitsToDouble((long)r.ReadBits(64)); break;
                case TypeCode.Char: o = (char)r.ReadBits(16); break;
                case TypeCode.String: o = ReadString(r); break;
            }
            if (o != null) return orig.IsEnum ? Enum.ToObject(orig, o) : o;
            if (t == typeof(Vector2)) return new Vector2(r.ReadF32(), r.ReadF32());
            if (t == typeof(Vector3)) return new Vector3(r.ReadF32(), r.ReadF32(), r.ReadF32());
            if (t == typeof(Quaternion)) return new Quaternion(r.ReadF32(), r.ReadF32(), r.ReadF32(), r.ReadF32());
            if (t == typeof(Color)) return new Color(r.ReadF32(), r.ReadF32(), r.ReadF32(), r.ReadF32());
            if (t == typeof(PlayerID)) return new PlayerID((ushort)r.ReadBits(16));
            if (t == typeof(byte[])) { int n = (int)r.ReadUInt(20); if (n > MaxBytes) throw new NetException("byte[] too long"); return r.ReadBytes(n); }
            if (typeof(NetworkIdentity).IsAssignableFrom(t)) { uint id = (uint)r.ReadBits(32); return id == 0 ? null : ResolveIdentity?.Invoke(id); }
            if (typeof(INetSerializable).IsAssignableFrom(t))
            {
                if (!r.ReadBool()) return null;
                var s = (INetSerializable)Activator.CreateInstance(t); s.Read(r); return s;
            }
            if (t.IsArray)
            {
                int n = (int)r.ReadUInt(16); var et = t.GetElementType(); var a = Array.CreateInstance(et, n);
                for (int i = 0; i < n; i++) a.SetValue(Read(r, et), i);
                return a;
            }
            if (t.IsGenericType && t.GetGenericTypeDefinition() == typeof(List<>))
            {
                int n = (int)r.ReadUInt(16); var et = t.GetGenericArguments()[0]; var l = (IList)Activator.CreateInstance(t);
                for (int i = 0; i < n; i++) l.Add(Read(r, et));
                return l;
            }
            if (t.IsGenericType && t.GetGenericTypeDefinition() == typeof(Nullable<>))
                return r.ReadBool() ? Read(r, Nullable.GetUnderlyingType(t)) : null;
            throw new NetException("cannot deserialize " + t.FullName + " (implement INetSerializable)");
        }

        public static void Write<T>(BitWriter w, T v) => Write(w, v, typeof(T));
        public static T Read<T>(BitReader r) => (T)Read(r, typeof(T));

        public static void WriteString(BitWriter w, string s)
        {
            var b = System.Text.Encoding.UTF8.GetBytes(s ?? "");
            if (b.Length > MaxString) throw new NetException("string too long");
            w.WriteUInt((ulong)b.Length, 16); w.WriteBytes(b);
        }

        public static string ReadString(BitReader r)
        {
            int n = (int)r.ReadUInt(16);
            return System.Text.Encoding.UTF8.GetString(r.ReadBytes(n));
        }

        public static bool AreEqual<T>(T a, T b) => EqualityComparer<T>.Default.Equals(a, b);
    }
}
