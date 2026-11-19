using System;
using System.IO;
using System.Linq;
using System.Reflection;
using UnityEditor;
using UnityEditor.SceneManagement;
using UnityEngine;

// Entry points for `Unity -batchmode -executeMethod`: each logs what it found and leaves the
// editor with code 0 when the check passed and 1 when it did not.
public static class AurixBuildCheck
{
    static void Leave(string check, bool passed, string detail)
    {
        Debug.Log("AURIX " + check + (passed ? " PASS " : " FAIL ") + detail);
        EditorApplication.Exit(passed ? 0 : 1);
    }

    // The package compiled (the editor does not get here otherwise) and its post-processor wove the RPCs.
    public static void Compile()
    {
        var names = AppDomain.CurrentDomain.GetAssemblies().Select(assembly => assembly.GetName().Name).ToArray();
        var missing = new[] { "Aurix.Net", "Aurix.Net.Editor" }.Where(name => !names.Contains(name)).ToArray();
        var probe = typeof(WeaveProbe);
        bool marker = probe.Assembly.GetTypes().Any(type => type.Name == "AurixNetWoven");
        bool body = probe.GetMethod("CmdHit__Impl", BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic) != null;
        Leave(
            "compile",
            missing.Length == 0 && marker && body,
            "editor " + Application.unityVersion
                + ", missing assemblies [" + string.Join(",", missing) + "]"
                + ", woven marker " + marker + ", woven rpc body " + body
        );
    }

    // A player of the editor's own platform with the probe in its scene builds.
    public static void BuildPlayer() => Build(EditorUserBuildSettings.activeBuildTarget, "player");

    // A Web player builds. It is not compressed, so a server that sets no headers can show its page.
    public static void BuildWeb()
    {
        PlayerSettings.WebGL.compressionFormat = WebGLCompressionFormat.Disabled;
        Build(BuildTarget.WebGL, "web");
    }

    static void Build(BuildTarget target, string check)
    {
        var scene = EditorSceneManager.NewScene(NewSceneSetup.DefaultGameObjects, NewSceneMode.Single);
        new GameObject("probe").AddComponent<WeaveProbe>();
        new GameObject("web").AddComponent<WebProbe>();
        Directory.CreateDirectory("Assets/Scenes");
        EditorSceneManager.SaveScene(scene, "Assets/Scenes/Probe.unity");
        var report = BuildPipeline.BuildPlayer(
            new BuildPlayerOptions
            {
                scenes = new[] { "Assets/Scenes/Probe.unity" },
                locationPathName = "Build/" + target + "/probe" + (target == BuildTarget.StandaloneWindows64 ? ".exe" : ""),
                target = target,
                options = BuildOptions.None,
            }
        );
        foreach (var step in report.steps)
        foreach (var message in step.messages.Where(message => message.type == LogType.Warning))
            Debug.Log("AURIX " + check + " WARN " + step.name + ": " + message.content.Split('\n')[0]);
        Leave(
            check,
            report.summary.result == UnityEditor.Build.Reporting.BuildResult.Succeeded,
            target + ": " + report.summary.result + ", errors " + report.summary.totalErrors + ", warnings " + report.summary.totalWarnings
        );
    }
}
