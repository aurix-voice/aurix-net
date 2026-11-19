// Component values and their bit codec (full and delta), mirrors net-wire::value.
using System;
using System.Text;

namespace Aurix.Net.Managed
{
    /// <summary>One field value; which member is used depends on the field type.</summary>
    public struct Value
    {
        public bool B;
        public ulong U;
        public long I;
        public float F;
        public float X,
            Y,
            Z,
            W;
        public string S;
        public byte[] Bytes;

        public override bool Equals(object target) =>
            target is Value value
            && B == value.B
            && U == value.U
            && I == value.I
            && F == value.F
            && X == value.X
            && Y == value.Y
            && Z == value.Z
            && W == value.W
            && S == value.S
            && SameBytes(Bytes, value.Bytes);

        public override int GetHashCode() =>
            (int)U ^ I.GetHashCode() ^ F.GetHashCode() ^ X.GetHashCode();

        static bool SameBytes(byte[] left, byte[] right)
        {
            if (left == null || right == null)
            {
                return (left?.Length ?? 0) == (right?.Length ?? 0);
            }

            if (left.Length != right.Length)
            {
                return false;
            }

            for (int i = 0; i < left.Length; i++)
            {
                if (left[i] != right[i])
                {
                    return false;
                }
            }

            return true;
        }

        public static Value Zero(FieldType type) =>
            type.Kind == FieldKind.Quat ? new Value { W = 1 }
            : type.Kind == FieldKind.String ? new Value { S = "" }
            : type.Kind == FieldKind.Bytes ? new Value { Bytes = Array.Empty<byte>() }
            : default;
    }

    public sealed class ComponentValue
    {
        public Value[] Fields;

        public static ComponentValue Zero(ComponentDef component)
        {
            var value = new ComponentValue { Fields = new Value[component.Fields.Count] };
            for (int i = 0; i < component.Fields.Count; i++)
            {
                value.Fields[i] = Value.Zero(component.Fields[i].Type);
            }

            return value;
        }

        public ComponentValue Clone() => new ComponentValue { Fields = (Value[])Fields.Clone() };

        public static void WriteField(BitWriter writer, FieldType type, in Value value)
        {
            const float lim = 0.70710677f;
            switch (type.Kind)
            {
                case FieldKind.Bool:
                    writer.WriteBool(value.B);
                    break;
                case FieldKind.UInt:
                    writer.WriteUInt(value.U, type.Bits);
                    break;
                case FieldKind.Int:
                    writer.WriteInt(value.I, type.Bits);
                    break;
                case FieldKind.F32:
                    writer.WriteF32(value.F);
                    break;
                case FieldKind.Quant:
                    writer.WriteQuantized(value.F, type.Min, type.Max, type.Res);
                    break;
                case FieldKind.Vec2:
                    writer.WriteQuantized(value.X, type.Min, type.Max, type.Res);
                    writer.WriteQuantized(value.Y, type.Min, type.Max, type.Res);
                    break;
                case FieldKind.Vec3:
                    writer.WriteQuantized(value.X, type.Min, type.Max, type.Res);
                    writer.WriteQuantized(value.Y, type.Min, type.Max, type.Res);
                    writer.WriteQuantized(value.Z, type.Min, type.Max, type.Res);
                    break;
                case FieldKind.Quat:
                {
                    float x = value.X,
                        y = value.Y,
                        z = value.Z,
                        ww = value.W;
                    float len = (float)Math.Sqrt(x * x + y * y + z * z + ww * ww);
                    float[] normalized =
                        len > 0
                            ? new[] { x / len, y / len, z / len, ww / len }
                            : new[] { 0f, 0f, 0f, 1f };
                    int largest = 0;
                    for (int i = 1; i < 4; i++)
                    {
                        if (Math.Abs(normalized[i]) > Math.Abs(normalized[largest]))
                        {
                            largest = i;
                        }
                    }

                    float sign = normalized[largest] < 0 ? -1 : 1;
                    writer.WriteUInt((ulong)largest, 2);
                    float res = 2 * lim / ((1 << 10) - 1);
                    for (int i = 0; i < 4; i++)
                    {
                        if (i != largest)
                        {
                            writer.WriteQuantized(normalized[i] * sign, -lim, lim, res);
                        }
                    }

                    break;
                }
                case FieldKind.String:
                {
                    var bytes = Encoding.UTF8.GetBytes(value.S ?? "");
                    if (bytes.Length > type.Max_)
                    {
                        bytes = TruncateUtf8(bytes, (int)type.Max_);
                    }

                    writer.WriteUInt((ulong)bytes.Length, Bits.LenBits(type.Max_));
                    writer.WriteBytes(bytes);
                    break;
                }
                case FieldKind.Bytes:
                {
                    var bytes = value.Bytes ?? Array.Empty<byte>();
                    if (bytes.Length > type.Max_)
                    {
                        Array.Resize(ref bytes, (int)type.Max_);
                    }

                    writer.WriteUInt((ulong)bytes.Length, Bits.LenBits(type.Max_));
                    writer.WriteBytes(bytes);
                    break;
                }
            }
        }

        static byte[] TruncateUtf8(byte[] bytes, int max)
        {
            int count = max;
            while (count > 0 && (bytes[count] & 0xC0) == 0x80)
            {
                count--;
            }

            Array.Resize(ref bytes, count);
            return bytes;
        }

        public static Value ReadField(BitReader reader, FieldType type)
        {
            const float lim = 0.70710677f;
            switch (type.Kind)
            {
                case FieldKind.Bool:
                    return new Value { B = reader.ReadBool() };
                case FieldKind.UInt:
                    return new Value { U = reader.ReadUInt(type.Bits) };
                case FieldKind.Int:
                    return new Value { I = reader.ReadInt(type.Bits) };
                case FieldKind.F32:
                    return new Value { F = reader.ReadF32() };
                case FieldKind.Quant:
                    return new Value { F = reader.ReadQuantized(type.Min, type.Max, type.Res) };
                case FieldKind.Vec2:
                    return new Value
                    {
                        X = reader.ReadQuantized(type.Min, type.Max, type.Res),
                        Y = reader.ReadQuantized(type.Min, type.Max, type.Res),
                    };
                case FieldKind.Vec3:
                    return new Value
                    {
                        X = reader.ReadQuantized(type.Min, type.Max, type.Res),
                        Y = reader.ReadQuantized(type.Min, type.Max, type.Res),
                        Z = reader.ReadQuantized(type.Min, type.Max, type.Res),
                    };
                case FieldKind.Quat:
                {
                    int largest = (int)reader.ReadUInt(2);
                    float res = 2 * lim / ((1 << 10) - 1);
                    var normalized = new float[4];
                    float sum = 0;
                    for (int i = 0; i < 4; i++)
                    {
                        if (i != largest)
                        {
                            normalized[i] = reader.ReadQuantized(-lim, lim, res);
                            sum += normalized[i] * normalized[i];
                        }
                    }

                    normalized[largest] = (float)Math.Sqrt(Math.Max(0, 1 - sum));
                    return new Value
                    {
                        X = normalized[0],
                        Y = normalized[1],
                        Z = normalized[2],
                        W = normalized[3],
                    };
                }
                case FieldKind.String:
                {
                    int count = (int)reader.ReadUInt(Bits.LenBits(type.Max_));
                    return new Value { S = Encoding.UTF8.GetString(reader.ReadBytes(count)) };
                }
                case FieldKind.Bytes:
                {
                    int count = (int)reader.ReadUInt(Bits.LenBits(type.Max_));
                    return new Value { Bytes = reader.ReadBytes(count) };
                }
            }
            throw new NetException("bad field type");
        }

        public void EncodeFull(ComponentDef component, BitWriter writer)
        {
            for (int i = 0; i < component.Fields.Count; i++)
            {
                WriteField(writer, component.Fields[i].Type, Fields[i]);
            }
        }

        public static ComponentValue DecodeFull(ComponentDef component, BitReader reader)
        {
            var value = new ComponentValue { Fields = new Value[component.Fields.Count] };
            for (int i = 0; i < component.Fields.Count; i++)
            {
                value.Fields[i] = ReadField(reader, component.Fields[i].Type);
            }

            return value;
        }

        public static ComponentValue DecodeDelta(
            ComponentValue @base,
            ComponentDef component,
            BitReader reader
        )
        {
            var changed = new bool[component.Fields.Count];
            for (int i = 0; i < changed.Length; i++)
            {
                changed[i] = reader.ReadBool();
            }

            var copy = @base.Clone();
            for (int i = 0; i < changed.Length; i++)
            {
                if (changed[i])
                {
                    copy.Fields[i] = ReadField(reader, component.Fields[i].Type);
                }
            }

            return copy;
        }
    }
}
