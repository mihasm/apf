-- Loads the plugin in REAPER in every format it was built as (CLAP, VST3, AU), plays the
-- same chord through each on its own track, renders each track to a WAV file and quits.
-- Run by scripts/reaper-render-test.sh, which then quits REAPER and checks the files.
--
-- Arguments come through the environment:
--   APF_REAPER_PLUGIN  the plugin's name as REAPER lists it, e.g. "APF Demo Synth (Miha Smrekar)"
--   APF_REAPER_OUT     folder for the results

local name = os.getenv("APF_REAPER_PLUGIN")
local out = os.getenv("APF_REAPER_OUT")
local report = io.open(out .. "/report.txt", "w")
local function say(s) report:write(s .. "\n"); report:flush() end

local formats = { { "CLAP", "CLAPi: " }, { "VST3", "VST3i: " }, { "AU", "AUi: " } }
local au_name = os.getenv("APF_REAPER_AU_NAME") -- AUs are listed as "Vendor: Name"

reaper.PreventUIRefresh(1)
for i, f in ipairs(formats) do
  reaper.InsertTrackAtIndex(i - 1, false)
  local track = reaper.GetTrack(0, i - 1)
  reaper.GetSetMediaTrackInfo_String(track, "P_NAME", f[1], true)
  local fxname = f[2] .. ((f[1] == "AU" and au_name) or name)
  local fx = reaper.TrackFX_AddByName(track, fxname, false, -1)
  if fx < 0 then
    say(f[1] .. ": NOT FOUND (" .. fxname .. ")")
  else
    local _, loaded = reaper.TrackFX_GetFXName(track, fx, "")
    say(f[1] .. ": loaded '" .. loaded .. "', " .. reaper.TrackFX_GetNumParams(track, fx) .. " parameters")
    -- Set the cutoff through the host, as automation would.
    for p = 0, reaper.TrackFX_GetNumParams(track, fx) - 1 do
      local _, pname = reaper.TrackFX_GetParamName(track, fx, p, "")
      if pname == "Cutoff" then
        reaper.TrackFX_SetParamNormalized(track, fx, p, 0.6)
        local _, text = reaper.TrackFX_GetFormattedParamValue(track, fx, p, "")
        say(f[1] .. ": host set Cutoff to " .. text)
      end
    end
  end
  local item = reaper.CreateNewMIDIItemInProj(track, 0, 2.0, false)
  local take = reaper.GetActiveTake(item)
  for _, key in ipairs({ 57, 60, 64 }) do
    reaper.MIDI_InsertNote(take, false, false, 0, reaper.MIDI_GetPPQPosFromProjTime(take, 1.5), 0, key, 100, true)
  end
  reaper.MIDI_Sort(take)
end
reaper.PreventUIRefresh(-1)

-- Render each track as a stem: <out>/<track name>.wav, 48 kHz.
reaper.GetSetProjectInfo(0, "RENDER_SETTINGS", 3, true)   -- stems (selected tracks)
reaper.GetSetProjectInfo(0, "RENDER_BOUNDSFLAG", 1, true) -- entire project
reaper.GetSetProjectInfo(0, "RENDER_SRATE", 48000, true)
reaper.GetSetProjectInfo(0, "RENDER_CHANNELS", 2, true)
reaper.GetSetProjectInfo_String(0, "RENDER_FILE", out, true)
reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", "$track", true)
reaper.GetSetProjectInfo_String(0, "RENDER_FORMAT", "evaw", true) -- WAV
for i = 0, reaper.CountTracks(0) - 1 do reaper.SetTrackSelected(reaper.GetTrack(0, i), true) end
reaper.Main_OnCommand(42230, 0) -- render project, using the most recent render settings, auto-close
reaper.Main_SaveProjectEx(0, out .. "/test.rpp", 0)
say("rendered")
report:close()
