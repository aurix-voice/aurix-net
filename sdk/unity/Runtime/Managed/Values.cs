// Component values and their bit codec (full and delta), mirrors net-wire::value.
using System;
using System.Text;

namespace Aurix.Net.Managed
{
    /// <summary>One field value; which member is used depends on the field type.</summary>
    public struct Value
    {
        public bool B; public ulong U; public long I; public float F; public float X, Y, Z, W; public string S; public byte[] Bytes;
        public override bool Equals(object o) => o is Value v && B == v.B && U == v.U && I == v.I && F == v.F && X == v.X && Y == v.Y && Z == v.Z && W == v.W && S == v.S && SameBytes(Bytes, v.Bytes);
        public override int GetHashCode() => (int)U ^ I.GetHashCode() ^ F.GetHashCode() ^ X.GetHashCode();
        static bool SameBytes(byte[] a, byte[] b) { if (a == null || b == null) return (a?.Length ?? 0) == (b?.Length ?? 0); if (a.Length != b.Length) return false; for (int i = 0; i < a.Length; i++) if (a[i] != b[i]) return false; return true; }
        public static Value Zero(FieldType t) => t.Kind == FieldKind.Quat ? new Value { W = 1 } : t.Kind == FieldKind.String ? new Value { S = "" } : t.Kind == FieldKind.Bytes ? new Value { Bytes = Array.Empty<byte>() } : default;
    }

    public sealed class ComponentValue
    {
        public Value[] Fields;
        public static ComponentValue Zero(ComponentDef c) { var v = new ComponentValue { Fields = new Value[c.Fields.Count] }; for (int i = 0; i < c.Fields.Count; i++) v.Fields[i] = Value.Zero(c.Fields[i].Type); return v; }
        public ComponentValue Clone() => new ComponentValue { Fields = (Value[])Fields.Clone() };

        public static void WriteField(BitWriter w, FieldType t, in Value v)
        {
            const float lim = 0.70710677f;
            switch (t.Kind)
            {
                case FieldKind.Bool: w.WriteBool(v.B); break;
                case FieldKind.UInt: w.WriteUInt(v.U, t.Bits); break;
                case FieldKind.Int: w.WriteInt(v.I, t.Bits); break;
                case FieldKind.F32: w.WriteF32(v.F); break;
                case FieldKind.Quant: w.WriteQuantized(v.F, t.Min, t.Max, t.Res); break;
                case FieldKind.Vec2: w.WriteQuantized(v.X, t.Min, t.Max, t.Res); w.WriteQuantized(v.Y, t.Min, t.Max, t.Res); break;
                case FieldKind.Vec3: w.WriteQuantized(v.X, t.Min, t.Max, t.Res); w.WriteQuantized(v.Y, t.Min, t.Max, t.Res); w.WriteQuantized(v.Z, t.Min, t.Max, t.Res); break;
                case FieldKind.Quat:
                    {
                        float x = v.X, y = v.Y, z = v.Z, ww = v.W; float len = (float)Math.Sqrt(x * x + y * y + z * z + ww * ww);
                        float[] q = len > 0 ? new[] { x / len, y / len, z / len, ww / len } : new[] { 0f, 0f, 0f, 1f };
                        int largest = 0; for (int i = 1; i < 4; i++) if (Math.Abs(q[i]) > Math.Abs(q[largest])) largest = i;
                        float sign = q[largest] < 0 ? -1 : 1; w.WriteUInt((ulong)largest, 2);
                        float res = 2 * lim / ((1 << 10) - 1);
                        for (int i = 0; i < 4; i++) if (i != largest) w.WriteQuantized(q[i] * sign, -lim, lim, res);
                        break;
                    }
                case FieldKind.String: { var b = Encoding.UTF8.GetBytes(v.S ?? ""); if (b.Length > t.Max_) b = TruncateUtf8(b, (int)t.Max_); w.WriteUInt((ulong)b.Length, Bits.LenBits(t.Max_)); w.WriteBytes(b); break; }
                case FieldKind.Bytes: { var b = v.Bytes ?? Array.Empty<byte>(); if (b.Length > t.Max_) Array.Resize(ref b, (int)t.Max_); w.WriteUInt((ulong)b.Length, Bits.LenBits(t.Max_)); w.WriteBytes(b); break; }
            }
        }
        static byte[] TruncateUtf8(byte[] b, int max) { int n = max; while (n > 0 && (b[n] & 0xC0) == 0x80) n--; Array.Resize(ref b, n); return b; }

        public static Value ReadField(BitReader r, FieldType t)
        {
            const float lim = 0.70710677f;
            switch (t.Kind)
            {
                case FieldKind.Bool: return new Value { B = r.ReadBool() };
                case FieldKind.UInt: return new Value { U = r.ReadUInt(t.Bits) };
                case FieldKind.Int: return new Value { I = r.ReadInt(t.Bits) };
                case FieldKind.F32: return new Value { F = r.ReadF32() };
                case FieldKind.Quant: return new Value { F = r.ReadQuantized(t.Min, t.Max, t.Res) };
                case FieldKind.Vec2: return new Value { X = r.ReadQuantized(t.Min, t.Max, t.Res), Y = r.ReadQuantized(t.Min, t.Max, t.Res) };
                case FieldKind.Vec3: return new Value { X = r.ReadQuantized(t.Min, t.Max, t.Res), Y = r.ReadQuantized(t.Min, t.Max, t.Res), Z = r.ReadQuantized(t.Min, t.Max, t.Res) };
                case FieldKind.Quat:
                    {
                        int largest = (int)r.ReadUInt(2); float res = 2 * lim / ((1 << 10) - 1); var q = new float[4]; float sum = 0;
                        for (int i = 0; i < 4; i++) if (i != largest) { q[i] = r.ReadQuantized(-lim, lim, res); sum += q[i] * q[i]; }
                        q[largest] = (float)Math.Sqrt(Math.Max(0, 1 - sum));
                        return new Value { X = q[0], Y = q[1], Z = q[2], W = q[3] };
                    }
                case FieldKind.String: { int n = (int)r.ReadUInt(Bits.LenBits(t.Max_)); return new Value { S = Encoding.UTF8.GetString(r.ReadBytes(n)) }; }
                case FieldKind.Bytes: { int n = (int)r.ReadUInt(Bits.LenBits(t.Max_)); return new Value { Bytes = r.ReadBytes(n) }; }
            }
            throw new NetException("bad field type");
        }

        public void EncodeFull(ComponentDef c, BitWriter w) { for (int i = 0; i < c.Fields.Count; i++) WriteField(w, c.Fields[i].Type, Fields[i]); }
        public static ComponentValue DecodeFull(ComponentDef c, BitReader r) { var v = new ComponentValue { Fields = new Value[c.Fields.Count] }; for (int i = 0; i < c.Fields.Count; i++) v.Fields[i] = ReadField(r, c.Fields[i].Type); return v; }
        public static ComponentValue DecodeDelta(ComponentValue b, ComponentDef c, BitReader r)
        {
            var changed = new bool[c.Fields.Count]; for (int i = 0; i < changed.Length; i++) changed[i] = r.ReadBool();
            var o = b.Clone(); for (int i = 0; i < changed.Length; i++) if (changed[i]) o.Fields[i] = ReadField(r, c.Fields[i].Type);
            return o;
        }
    }
}
