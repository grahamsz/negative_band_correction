"use strict";
// Run with the installed UXP Developer Tool's Electron executable in Node mode.
// Uses Adobe's own package implementation, including manifest/icon validation.
const fs=require("fs"),path=require("path"),os=require("os");
const root=path.resolve(__dirname,"..");
const udt=process.env.UXP_DEVELOPER_TOOLS || "C:/Program Files/Adobe/Adobe UXP Developer Tools";
const packagingCore=process.env.UXP_PACKAGING_CORE || path.join(udt,"resources/app.asar/node_modules/@adobe/uxp-devtools-core");
const PackageCommand=require(path.join(packagingCore,"src/core/client/plugin/actions/PluginPackageCommand.js"));
const plugin=path.join(root,"dist/plugin"), output=path.join(root,"dist");
const manifest=path.join(plugin,"manifest.json");
const addon=JSON.parse(fs.readFileSync(manifest)).addon.name;
if(!fs.existsSync(path.join(plugin,"win/x64",addon)))throw new Error("Build the native addon first: "+addon);
// Loaded historical addons can be locked in the development bundle. Package
// only the current manifest's addon and the supported panel files.
const staging=fs.mkdtempSync(path.join(os.tmpdir(),"banding-package-"));
for(const name of ["manifest.json","index.html","style.css","main.js","workflow.js","native-client.js","compact-stack.js","LICENSE-MIT","LICENSE-APACHE","UPSTREAM.md"])
    fs.copyFileSync(path.join(plugin,name),path.join(staging,name));
fs.mkdirSync(path.join(staging,"icons"));
for(const name of fs.readdirSync(path.join(plugin,"icons")))
    fs.copyFileSync(path.join(plugin,"icons",name),path.join(staging,"icons",name));
fs.mkdirSync(path.join(staging,"win/x64"),{recursive:true});
fs.copyFileSync(path.join(plugin,"win/x64",addon),path.join(staging,"win/x64",addon));
const command=new PackageCommand({}, {manifest:path.join(staging,"manifest.json"),packageDir:output,apps:["PS"]});
command.package().then(results=>{
    for(const result of results) if(!result.success)throw result.error;
    console.log("Packaged with Adobe UXP Developer Tool: "+path.join(output,JSON.parse(fs.readFileSync(manifest)).id+"_PS.ccx"));
    console.log("Offline package validation passed. Verify installation and native loading in Photoshop before release.");
}).catch(error=>{console.error(error);process.exitCode=1;})
    .finally(()=>fs.rmdirSync(staging,{recursive:true}));
