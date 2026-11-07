// Minimal UnityEngine stand-ins so the Unity layer compiles under plain dotnet (no behaviour).
using System;
using System.Collections.Generic;
namespace UnityEngine
{
    public class Object { public string name; public static T Instantiate<T>(T o) where T : Object => o; public static void Destroy(Object o) { } }
    public class GameObject : Object { }
    public class Component : Object { public Transform transform = new Transform(); public GameObject gameObject = new GameObject(); public void GetComponentsInChildren<T>(bool inc, List<T> r) { } }
    public class Behaviour : Component { }
    public class MonoBehaviour : Behaviour { }
    public class ScriptableObject : Object { }
    public class Transform : Component { public Vector3 position; public Quaternion rotation; }
    public struct Vector2 { public float x, y; public Vector2(float a, float b) { x = a; y = b; } }
    public struct Vector3 { public float x, y, z; public Vector3(float a, float b, float c) { x = a; y = b; z = c; } public static float Distance(Vector3 a, Vector3 b) => 0; public static Vector3 Lerp(Vector3 a, Vector3 b, float t) => b; }
    public struct Quaternion { public float x, y, z, w; public Quaternion(float a, float b, float c, float d) { x = a; y = b; z = c; w = d; } public static Quaternion identity => new Quaternion(0, 0, 0, 1); public static Quaternion Slerp(Quaternion a, Quaternion b, float t) => b; }
    public static class Mathf { public static float Clamp01(float v) => v < 0 ? 0 : v > 1 ? 1 : v; public static float Max(float a, float b) => a > b ? a : b; }
    public static class Time { public static float unscaledDeltaTime => 0.016f; public static float unscaledTime => 0; }
    public static class Debug { public static void Log(object o) { Console.WriteLine(o); } public static void LogWarning(object o) { Console.WriteLine(o); } public static void LogError(object o) { Console.WriteLine(o); } }
    public class TooltipAttribute : Attribute { public TooltipAttribute(string s) { } }
    public class TextAreaAttribute : Attribute { public TextAreaAttribute(int a, int b) { } }
    public class SerializeFieldAttribute : Attribute { }
    public class CreateAssetMenuAttribute : Attribute { public string menuName, fileName; }
    public class DisallowMultipleComponentAttribute : Attribute { }
    public class DefaultExecutionOrderAttribute : Attribute { public DefaultExecutionOrderAttribute(int o) { } }
}
