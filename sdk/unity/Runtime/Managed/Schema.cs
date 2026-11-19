// IDL parser, same grammar as net-wire::schema (one statement per line, // comments).
using System;
using System.Collections.Generic;
using System.Globalization;

namespace Aurix.Net.Managed
{
    public enum FieldKind
    {
        Bool,
        UInt,
        Int,
        F32,
        Quant,
        Vec2,
        Vec3,
        Quat,
        String,
        Bytes,
    }

    public sealed class FieldType
    {
        public FieldKind Kind;
        public int Bits;
        public float Min,
            Max,
            Res;
        public uint Max_;
    }

    public sealed class Field
    {
        public string Name;
        public FieldType Type;
    }

    public sealed class ComponentDef
    {
        public ushort Id;
        public string Name;
        public bool OwnerOnly,
            Global;
        public List<Field> Fields = new List<Field>();

        public int IndexOf(string field)
        {
            for (int i = 0; i < Fields.Count; i++)
            {
                if (Fields[i].Name == field)
                {
                    return i;
                }
            }

            return -1;
        }
    }

    public sealed class Schema
    {
        public List<ComponentDef> Components = new List<ComponentDef>();

        public ComponentDef ById(int id) =>
            id >= 0 && id < Components.Count ? Components[id] : null;

        public ComponentDef ByName(string name)
        {
            foreach (var component in Components)
            {
                if (component.Name == name)
                {
                    return component;
                }
            }

            return null;
        }

        public int ComponentIdBits =>
            Math.Max(1, 64 - Bits.LeadingZeros((ulong)Math.Max(2, Components.Count) - 1));

        public static Schema Parse(string text)
        {
            var schema = new Schema();
            ComponentDef cur = null;
            int line = 0;
            foreach (var raw in text.Split('\n'))
            {
                line++;
                var source = raw;
                int commentStart = source.IndexOf("//", StringComparison.Ordinal);
                if (commentStart >= 0)
                {
                    source = source.Substring(0, commentStart);
                }

                source = source.Trim();
                if (source.Length == 0)
                {
                    continue;
                }

                if (source.StartsWith("component "))
                {
                    if (cur != null)
                    {
                        throw new NetException($"schema line {line}: nested component");
                    }

                    var rest = source.Substring(10).Trim();
                    if (!rest.EndsWith("{"))
                    {
                        throw new NetException($"schema line {line}: expected '{{'");
                    }

                    var parts = rest.Substring(0, rest.Length - 1)
                        .Trim()
                        .Split(new[] { ' ', '\t' }, StringSplitOptions.RemoveEmptyEntries);
                    cur = new ComponentDef
                    {
                        Id = (ushort)schema.Components.Count,
                        Name = parts[0],
                    };
                    for (int i = 1; i < parts.Length; i++)
                    {
                        if (parts[i] == "@owner_only")
                        {
                            cur.OwnerOnly = true;
                        }
                        else if (parts[i] == "@global")
                        {
                            cur.Global = true;
                        }
                        else
                        {
                            throw new NetException($"schema line {line}: unknown flag {parts[i]}");
                        }
                    }
                    continue;
                }
                if (source == "}")
                {
                    if (cur == null)
                    {
                        throw new NetException($"schema line {line}: stray '}}'");
                    }

                    schema.Components.Add(cur);
                    cur = null;
                    continue;
                }
                if (cur == null)
                {
                    throw new NetException($"schema line {line}: field outside component");
                }

                int colon = source.IndexOf(':');
                if (colon < 0)
                {
                    throw new NetException($"schema line {line}: expected name: type");
                }

                var name = source.Substring(0, colon).Trim();
                var ty = source.Substring(colon + 1).Trim();
                string attr = null;
                int at = ty.IndexOf('@');
                if (at >= 0)
                {
                    attr = ty.Substring(at + 1).Trim();
                    ty = ty.Substring(0, at).Trim();
                }
                cur.Fields.Add(new Field { Name = name, Type = ParseType(ty, attr, line) });
            }
            if (cur != null)
            {
                throw new NetException("schema: unterminated component");
            }

            return schema;
        }

        static FieldType ParseType(string baseName, string attr, int line)
        {
            FieldType Range(FieldKind kind)
            {
                if (attr == null || !attr.StartsWith("range(") || !attr.EndsWith(")"))
                {
                    throw new NetException(
                        $"schema line {line}: {baseName} needs @range(min, max, res)"
                    );
                }

                var parts = attr.Substring(6, attr.Length - 7).Split(',');
                if (parts.Length != 3)
                {
                    throw new NetException($"schema line {line}: bad range");
                }

                return new FieldType
                {
                    Kind = kind,
                    Min = F(parts[0]),
                    Max = F(parts[1]),
                    Res = F(parts[2]),
                };
            }
            float F(string text) => float.Parse(text.Trim(), CultureInfo.InvariantCulture);
            uint? Paren(string name)
            {
                if (!baseName.StartsWith(name))
                {
                    return null;
                }

                var result = baseName.Substring(name.Length);
                if (result.Length == 0)
                {
                    return 0;
                }

                if (
                    result.StartsWith("(")
                    && result.EndsWith(")")
                    && uint.TryParse(result.Substring(1, result.Length - 2).Trim(), out var value)
                )
                {
                    return value;
                }

                throw new NetException($"schema line {line}: bad {name} size");
            }
            switch (baseName)
            {
                case "bool":
                    return new FieldType { Kind = FieldKind.Bool };
                case "u8":
                    return new FieldType { Kind = FieldKind.UInt, Bits = 8 };
                case "u16":
                    return new FieldType { Kind = FieldKind.UInt, Bits = 16 };
                case "u32":
                    return new FieldType { Kind = FieldKind.UInt, Bits = 32 };
                case "u64":
                    return new FieldType { Kind = FieldKind.UInt, Bits = 64 };
                case "i8":
                    return new FieldType { Kind = FieldKind.Int, Bits = 8 };
                case "i16":
                    return new FieldType { Kind = FieldKind.Int, Bits = 16 };
                case "i32":
                    return new FieldType { Kind = FieldKind.Int, Bits = 32 };
                case "i64":
                    return new FieldType { Kind = FieldKind.Int, Bits = 64 };
                case "f32":
                    return attr == null
                        ? new FieldType { Kind = FieldKind.F32 }
                        : Range(FieldKind.Quant);
                case "vec3":
                    return Range(FieldKind.Vec3);
                case "vec2":
                    return Range(FieldKind.Vec2);
                case "quat":
                    return new FieldType { Kind = FieldKind.Quat };
            }
            var bits = Paren("bits");
            if (bits.HasValue)
            {
                if (bits == 0 || bits > 64)
                {
                    throw new NetException($"schema line {line}: bits(n) needs 1..=64");
                }

                return new FieldType { Kind = FieldKind.UInt, Bits = (int)bits.Value };
            }
            var str = Paren("string");
            if (str.HasValue)
            {
                return new FieldType
                {
                    Kind = FieldKind.String,
                    Max_ = Math.Min(Math.Max(str.Value, 1), 65535),
                };
            }

            var by = Paren("bytes");
            if (by.HasValue)
            {
                return new FieldType
                {
                    Kind = FieldKind.Bytes,
                    Max_ = Math.Min(Math.Max(by.Value, 1), 65535),
                };
            }

            throw new NetException($"schema line {line}: unknown type {baseName}");
        }
    }
}
